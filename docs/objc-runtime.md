# The Objective-C runtime bridge (`crates/runtime/src/hle/objc.rs`)

This document describes the Objective-C runtime surface the emulator
implements: what each entry point does, what the emulator's approximation is,
and why the approximation is safe in a High-Level Emulation setting. It is the
reference for the code; the summary in the README is the landing page.

## The original fault, and the shape of the fix

The boot log for *The Simpsons Arcade* ends with a fetch fault at
`0x00000000` after roughly two `_objc_msgSend` calls:

```text
Fetch fault at 0x00000000 (pc 0x00000000): unmapped read of 4 byte(s) at 0x00000000
...
     3  __Unwind_SjLj_Register
     2  _objc_msgSend
     1  __Znwm
```

Four failure modes can each produce exactly that signature:

1. **A `_OBJC_CLASS_$_*` import slot bound to a HLE trampoline.** The guest
   loads `__objc_classrefs` and hands the value to `objc_msgSend` as a
   *receiver*. A trampoline page, however, is a series of `udf` markers — not a
   `struct objc_class` — so every class-method dispatch on an imported UIKit /
   Foundation class dereferences garbage and can land on `0`.
2. **A missing or `NULL` IMP.** When method lookup fails — or simply returns
   a NULL implementation — nothing the emulator originally did checked the
   address before transferring control. Jumping to `IMP == 0` is a fetch
   fault at `pc = 0` by definition.
3. **A lost host→guest return address.** The bridge executes resolved IMPs in
   the guest by rewriting the program counter and parking the caller's link
   register. When the shadow return bookkeeping got out of step, the method's
   `bx lr` walked into the void — again, `pc = 0`.
4. **A Thumb-mode corruption of the same path.** The link register of a Thumb
   caller has bit 0 set. Restoring the PC without the T bit re-interprets
   Thumb code as ARM and eventually faults with a strange address — or `0`.

The bridge addresses all four, in the order they matter:

```
_OSKlass_ msg          ┌─ nil receiver → ABI-exact zero (r0/r1, zeroed stret buffer)
                        ├─ guest metadata genuine classes read from __objc_classlist
objc_msgSend ──┬─ host class/instance → HOST_METHODS tables (UI/GL/objc)
               ├─ lookup_imp: image method_list_t walk, superclass chain, metaclass
               ├─ runtime answers: alloc/retain/class/respondsToSelector:/…
               ├─ superclass chain hits a host class → host table of that ancestor
               └─ unrecognised → log once (count) + answer nil — never pc = 0

control transfers ──►  validated IMP (mapped + executable) only
returns ──►            shadow return stack + jump_to (bit-0 T-bit restore)
```

No path on either side can install `0`, a HLE trampoline, or any
non-executable address as the next fetch address.

## Memory map

| region | address | contents |
|---|---|---|
| `hle` | `0x7000_0000 .. 0x7001_0000` | one 16-byte trampoline slot per imported function |
| `objc-classes` | `0x7001_0000 .. 0x7002_0000` | synthetic `struct objc_class`/`class_ro_t` for imported `_OBJC_CLASS_$_*` symbols |
| `host-objects` | `0x7002_0000 .. 0x7004_0000` | emulator-created classes (`objc_allocateClassPair`, Foundation factory objects, instances) |

## Registry state (`ObjcRuntime`, owned by `System`)

| field | contents |
|---|---|
| `names` | class/metaclass address → class name |
| `classes_by_name` | class name → class object address (`objc_getClass`, `NSClassFromString`) |
| `metaclass_of` | metaclass address → class address |
| `selectors`, `selector_names` | interned selector pointer ↔ name (`__objc_selrefs` + `sel_registerName`) |
| `host_imps` | `(class object, selector name) → IMP`, installed by `class_addMethod` |
| `associated` | `(owner, key) → value` for associated objects |
| `guest_calls`, `host_calls`, `missing` | dispatch counters for `--stats` |
| `unrecognized`, `nil_messages` | name → count, the failure tables |
| `last_dispatch` | one-line description of the most recent message; printed on any trap |

## Dispatch algorithm (`dispatch_with_start`)

```
Parameters : receiver, selector, start_class (= 0 unless a super send), stret_buffer
1. receiver == 0
     → count nil_messages[selector_name]++, answer 0 (r0=r1=0, zero 16 bytes at stret_buffer)
2. selector unreadable
     → count, answer 0 (never dereference an unreadable SEL further)
3. receiver is a registered host CLASS object
     → alloc/allocWithZone:/new → host_instance
     → else dispatch_host_method(class_name, receiver)
4. receiver's isa is a registered host class
     → dispatch_host_method
5. receiver_is_class := receiver's own storage parses as a class
6. start := start_class or isa(metaclass object for class-object receivers)
7. ok let &imp = host_imps[(receiver, name)] → call_method(imp)
8. ok let (imp, class_name) = lookup_imp(start, name) → call_method(imp)
9. runtime_answers(name) — NSObject bookkeeping — if matched → value
10. host_chain_lookup(start, name) — image chain ends in a synthetic superclass
      → dispatch_host_method(found_name, receiver)
11. otherwise: missing++, record "unrecognized selector -[Class sel]" → 0
```

`call_method` validates the target with `valid_method_target` (nonzero,
not in the trampoline page, mapped and execute-permitted) before
`Hle::call_guest`. `call_guest` pushes the current `lr` onto the shadow
return stack, sets `lr = HLE_RETURN`, jumps to the IMP (with Thumb-bit
restoration via `Cpu::jump_to`), and when the method returns the machine
pops the shadow stack and resumes the caller.

## Implemented entry points

### Messaging

| symbol | behaviour |
|---|---|
| `objc_msgSend` | full dispatch above |
| `objc_msgSend_stret` | ditto, with the struct-return buffer in `r0`, `self`/`_cmd` shifted one register; messaging nil zeroes the buffer (up to 16 bytes, never across a region) |
| `objc_msgSendSuper`, `objc_msgSendSuper_stret` | starts the search at `sup->super_class` (legacy `objc_super`) |
| `objc_msgSendSuper2`, `objc_msgSendSuper2_stret` | starts at `sup->current_class`'s superclass ("super2" ABI) |
| `objc_msgSend_fpret` | alias of `objc_msgSend` (on armv7, floats come back in core registers) |
| `objc_msgForward`, `objc_msgForward_stret` | reported (counted) and answered `0`; the emulator never forwards |

ABI note: the emulator's AAPCS frame at the trampoline means the guest's
arguments arrive exactly as r0–r3 read at the dispatch site — nothing is
repacked for variadic args anymore. The `stret` variants only matter for
the *address* the guest wants the struct written to.

### Class lookup and creation

| symbol | behaviour |
|---|---|
| `objc_getClass` / `objc_lookUpClass` | host + image registry lookup |
| `objc_getMetaClass` | metaclass of the found class |
| `objc_getRequiredClass` | like `objc_getClass`; on a miss the name is logged and a host stand-in class is *created* (a real runtime aborts, which is the least interesting thing a bring-up wants) |
| `objc_getClassList` / `objc_copyClassList` | enumerate the registry |
| `objc_allocateClassPair` / `objc_registerClassPair` / `objc_disposeClassPair` | synthesise a class (metaclass included); class-composition needs no further work |
| `objc_getProtocol` / `objc_copyProtocolList` | `0` (no protocol support yet) |

### Selectors

| symbol | behaviour |
|---|---|
| `sel_registerName` / `sel_getUid` / `objc_getSelector` | intern the name; returns a pointer that pointer-compares across the image (SEL is its own name string on this ABI) |
| `sel_getName` | SEL **is** the name pointer; returned unchanged |
| `sel_isEqual` | pointer equality, then name equality |
| `sel_isMapped` | `1` for any readable selector |

### Object / class / method reflection

`object_getClass`, `object_setClass`, `object_getClassName`, `object_copy`,
`object_dispose`, `object_getInstanceVariable`/`object_setInstanceVariable` (+ read of compile-time `class_ro_t` layout for `class_getName`-style APIs), and the `class_*` pair for superclasses, instance size, meta-class, method implementation, and method handles (`{ SEL; types; IMP }` triples in guest memory, so `method_setImplementation` patches a real slot).

### The rest

* retain/release era: `objc_retain` (+`Autorelease`, `AutoreleasedReturnValue`, `retainBlock`), `objc_release`, ARC `objc_storeStrong`/`objc_storeWeak`/`objc_initWeak`/`objc_loadWeak`/`objc_destroyWeak`/`objc_copyWeak`/`objc_moveWeak`
* properties: `objc_getProperty`, `objc_setProperty`, `objc_copyStruct`
* pools: `objc_autoreleasePoolPush`/`Pop` (tokens are real `NSAutoreleasePool` host instances)
* sync: `objc_sync_*` (no-ops returning 0)
* exceptions: `objc_exception_throw`/`_rethrow`/`objc_terminate` log and stop the run deterministically; `objc_begin_catch`/`objc_end_catch` satisfy the caller if the guest's own SjLj landing code enters the spotlight.
* associated objects: real storage in `ObjcRuntime::associated`
* class-composition (`class_addMethod`/`replaceMethod`), `method_exchangeImplementations`, `method_get*` — all over real method triples
* Foundation conveniences: `NSClassFromString`, `NSSelectorFromString`, `NSStringFromClass`, `NSStringFromSelector`
* `_objc_empty_cache`/`_objc_empty_vtable` (data symbols) answer 0 if the linker ever lets them run instead of returning a reference.

## Diagnostics

* `ObjcRuntime::last_dispatch` is refreshed on every message.
* A fetch-fault trap prints registers, the Objective-C summary, the shadow
  return stack depth/top-8, and the 32 most recent HLE calls.
* `--objc-trace` logs every dispatch (and every nil answer);
  `--objc-quiet` only records failures into the guest log.
* `--stats` prints the Objective-C report: `guest IMPs`, `host answers`,
  `missing`, selector/class registry sizes, plus the unrecognized-selector
  and nil-message tables.

## Tests

`crates/runtime/tests/objc.rs` hand-builds a two-class hierarchy in a real
Mach-O image (modern `class_ro_t` layout, 12-byte method entries) and dispatches
`_objc_msgSend` at it. Covered:

* a selector implemented by the class executes in the guest;
* superclass-chain inheritance finds the parent's IMP;
* messaging `nil` is a no-op, with the right counters;
* an unimplemented selector is recorded, not run;
* a NULL IMP in a method list cannot move the program counter to `0`;
* `retain`/`class` are answered by the runtime;
* an imported `_OBJC_CLASS_$_Widget` becomes a real class object that
  `+[Widget alloc]` works against.

## What is still missing

* `forwardInvocation:`/`methodSignatureForSelector:`-driven forwarding (the
  `unrecognized` path logs instead) — acceptable for the current coverage.
* Method caches per class (each send re-walks the chain — fine at
  thousands of sends/second; revisit if profiling says otherwise).
* `__OBJC,__protocol` registration and `protocol_get*` introspection.
* Blocks (`_NSConcreteBlock`) — the game does not use them.
