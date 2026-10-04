//! The machine: CPU + guest memory + the HLE environment, and the trap
//! dispatch loop that ties them together.

use arm::{Access, Cpu, Outcome, Trap};
use guestmem::AddressSpace;
use macho::MachO;

use crate::error::{describe_trap, Result, RuntimeError};
use crate::hle::objc;
use crate::hle::{self, System};
use crate::loader::{self, InitialStack, LoadOptions, LoadedImage, HLE_BASE, HLE_RETURN};
use crate::syscall;

/// Why the machine stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The guest called `exit`.
    Exited(i32),
    /// The instruction budget ran out.
    Budget,
    /// The interpreter hit something it cannot continue past.
    Trap(Trap),
    /// The guest asked to wait and nothing else can run (see [`crate::sched`]).
    Idle,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub instructions: u64,
    pub hle_calls: u64,
    pub syscalls: u64,
    pub memory_faults: u64,
    pub undefined_instructions: u64,
    /// Every HLE symbol called, with a count.
    pub hle_by_symbol: Vec<(String, u64)>,
}

#[derive(Debug)]
pub struct Machine {
    pub cpu: Cpu,
    pub mem: AddressSpace,
    pub image: LoadedImage,
    pub sys: System,
    pub stack: InitialStack,
    pub stats: Stats,
    pub stop: Option<StopReason>,
    /// Log every HLE call and syscall (`--trace`).
    pub trace: bool,
    /// Keep going past an undefined instruction, reporting it once.
    pub tolerate_undefined: bool,
    /// Instructions executed since the last frame was presented.
    pub since_present: u64,
}

impl Machine {
    /// Load an image and set up the initial thread state.
    pub fn boot(macho: MachO, options: &LoadOptions) -> Result<Machine> {
        let (image, mut mem) = loader::load(macho, options)?;
        let stack = loader::build_stack(&mut mem, options, image.stack_top)?;

        let mut cpu = Cpu::new();
        let thumb = image.entry & 1 != 0;
        cpu.reset(image.entry & !1, stack.sp, thumb);
        if thumb {
            // The entry's bit 0 selects Thumb; `reset` already applied it.
        }

        let mut sys = System::default();
        sys.args = std::iter::once(options.program_name.clone()).chain(options.args.iter().cloned()).collect();
        sys.bundle_path = std::path::Path::new(&options.program_name)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        Ok(Machine {
            cpu,
            mem,
            image,
            sys,
            stack,
            stats: Stats::default(),
            stop: None,
            trace: false,
            tolerate_undefined: false,
            since_present: 0,
        })
    }

    /// Run until the guest exits, the budget runs out, or a trap stops us.
    pub fn run(&mut self, budget: u64) -> Result<StopReason> {
        let mut executed = 0u64;
        loop {
            if let Some(reason) = self.stop.clone() {
                return Ok(reason);
            }
            if executed >= budget {
                return Ok(StopReason::Budget);
            }
            executed += 1;

            let pc = self.cpu.pc();

            // A call into the HLE trampoline page: run the host implementation.
            if let Some(symbol) = self.image.symbol_at_trampoline(pc) {
                let symbol = symbol.to_string();
                self.hle_dispatch(&symbol)?;
                self.since_present += 1;
                continue;
            }
            // A guest method returning into a host caller.
            if pc == HLE_RETURN {
                let target = self.sys.return_stack.pop().unwrap_or(0);
                self.cpu.set_pc(target);
                continue;
            }

            match self.cpu.step(&mut self.mem) {
                Outcome::Continue => {}
                Outcome::Trap(trap) => self.handle_trap(trap)?,
            }
            self.since_present += 1;
        }
    }

    fn handle_trap(&mut self, trap: Trap) -> Result<()> {
        match trap {
            Trap::Syscall { number } => {
                self.stats.syscalls += 1;
                if self.trace {
                    self.log(format!(
                        "svc #{number} at {:#010x} (r0={:#x} r1={:#x} r2={:#x})",
                        self.cpu.pc(),
                        self.cpu.r[0],
                        self.cpu.r[1],
                        self.cpu.r[2]
                    ));
                }
                if let Some(code) = syscall::dispatch(self, number)? {
                    self.sys.exit_code = Some(code);
                    self.sys.finished = true;
                    self.stop = Some(StopReason::Exited(code));
                }
            }
            Trap::HleCall { address } => {
                let symbol = self
                    .image
                    .symbol_at_trampoline(address)
                    .unwrap_or("<unknown>")
                    .to_string();
                self.hle_dispatch(&symbol)?;
            }
            Trap::SupervisorCall { immediate } => {
                self.log(format!("svc #{immediate:#x} (not the Darwin syscall ABI) at {:#010x}", self.cpu.pc()));
            }
            Trap::Breakpoint { address, imm } => {
                self.log(format!("breakpoint #{imm:#x} at {address:#010x} (lr {:#010x})", self.cpu.r[14]));
                self.stop = Some(StopReason::Trap(Trap::Breakpoint { address, imm }));
            }
            Trap::Undefined { address, insn, thumb } => {
                self.stats.undefined_instructions += 1;
                self.log(format!(
                    "undefined {} instruction {insn:#010x} at {address:#010x} (lr {:#010x})",
                    if thumb { "Thumb" } else { "ARM" },
                    self.cpu.r[14]
                ));
                if self.tolerate_undefined {
                    let size = if thumb && insn > 0xffff { 4 } else if thumb { 2 } else { 4 };
                    self.cpu.set_pc(address.wrapping_add(size));
                } else {
                    self.stop = Some(StopReason::Trap(Trap::Undefined { address, insn, thumb }));
                }
            }
            Trap::Memory { error, address, pc, access } => {
                self.stats.memory_faults += 1;
                self.log(format!(
                    "memory fault ({access:?}) at {address:#010x} from pc {pc:#010x}: {error}"
                ));
                if access == Access::Fetch && self.tolerate_undefined {
                    self.cpu.set_pc(pc.wrapping_add(if self.cpu.thumb() { 2 } else { 4 }));
                } else {
                    self.stop = Some(StopReason::Trap(Trap::Memory { error, address, pc, access }));
                }
            }
        }
        Ok(())
    }

    /// Call the HLE implementation of `symbol`.
    pub fn hle_dispatch(&mut self, symbol: &str) -> Result<()> {
        self.stats.hle_calls += 1;
        match self.stats.hle_by_symbol.iter_mut().find(|(name, _)| name == symbol) {
            Some((_, count)) => *count += 1,
            None => self.stats.hle_by_symbol.push((symbol.to_string(), 1)),
        }
        if self.trace {
            let name = hle::normalize(symbol).to_string();
            self.log(format!(
                "{name}({:#x}, {:#x}, {:#x}, {:#x}) lr={:#010x}",
                self.cpu.r[0],
                self.cpu.r[1],
                self.cpu.r[2],
                self.cpu.r[3],
                self.cpu.r[14]
            ));
        }
        let handler = hle::lookup(symbol);
        let return_address = self.cpu.r[14];
        let mut hle = hle::Hle {
            cpu: &mut self.cpu,
            mem: &mut self.mem,
            sys: &mut self.sys,
            symbol,
            jump: None,
        };
        let value = match handler {
            Some(handler) => handler(&mut hle)?,
            None => hle.unsupported(),
        };
        match hle.jump {
            Some(target) => {
                // Control was handed to the guest; anything it returns comes
                // back through `HLE_RETURN`.
                hle.cpu.set_pc(target);
            }
            None => {
                hle.cpu.r[0] = value;
                hle.cpu.set_pc(return_address);
            }
        }
        if self.sys.finished && self.stop.is_none() {
            let code = self.sys.exit_code.unwrap_or(0);
            self.stop = Some(StopReason::Exited(code));
        }
        Ok(())
    }

    /// Push a line into the guest's log (`--verbose`, strace, diagnostics).
    pub fn log(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.sys.log.push(message);
    }

    /// Convenience for tests and the CLI: an entry's address as a symbol name.
    pub fn describe_trap(&self, trap: &Trap) -> String {
        describe_trap(trap)
    }

    /// Call a guest function with up to four arguments, returning when it
    /// returns to `HLE_RETURN`.
    pub fn call_function(&mut self, address: u32, args: &[u32], budget: u64) -> Result<u32> {
        let saved = self.cpu.clone();
        let saved_branch = self.cpu.pc();
        self.sys.return_stack.push(0);
        self.cpu.r[14] = HLE_RETURN;
        for (i, value) in args.iter().enumerate().take(4) {
            self.cpu.r[i] = *value;
        }
        // r4-r11 must be preserved by the callee, so nothing to save there.
        self.cpu.set_pc(address & !1);
        if address & 1 != 0 {
            self.cpu.cpsr |= arm::FLAG_T;
        }

        let mut executed = 0u64;
        let result = loop {
            if executed >= budget {
                break Err(RuntimeError::Unsupported(format!(
                    "function at {address:#010x} did not return within {budget} instructions"
                )));
            }
            executed += 1;
            let pc = self.cpu.pc();
            if pc == HLE_RETURN {
                let value = self.cpu.r[0];
                break Ok(value);
            }
            if let Some(symbol) = self.image.symbol_at_trampoline(pc) {
                let symbol = symbol.to_string();
                self.hle_dispatch(&symbol)?;
                continue;
            }
            match self.cpu.step(&mut self.mem) {
                Outcome::Continue => {}
                Outcome::Trap(trap) => {
                    self.handle_trap(trap.clone())?;
                    if self.stop.is_some() {
                        break Err(RuntimeError::Trap(trap));
                    }
                }
            }
        };

        let result = result;
        let _ = saved_branch;
        self.cpu = saved;
        self.sys.return_stack.pop();
        result
    }

    /// The HLE page's base address (used by the loader and tests).
    pub fn hle_base(&self) -> u32 {
        HLE_BASE
    }

    /// Guest address of a host class, creating it if necessary.
    pub fn host_class(&mut self, name: &str) -> Result<u32> {
        let mut hle = hle::Hle {
            cpu: &mut self.cpu,
            mem: &mut self.mem,
            sys: &mut self.sys,
            symbol: "<host>",
            jump: None,
        };
        objc::host_class(&mut hle, name)
    }
}
