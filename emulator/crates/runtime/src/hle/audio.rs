//! AudioToolbox (AudioFile/AudioQueue), AudioSession, and OpenAL.
//!
//! The engine mixes its music and sound effects through AudioQueue and plays
//! them through OpenAL.  Neither is reproduced here yet: the emulator accepts
//! every call, hands back valid-looking handles and timestamps, and tracks how
//! many buffers were queued so a boot trace shows the audio path being reached.

use super::{Hle, HleFn};
use crate::error::Result;

pub const FUNCTIONS: &[(&str, HleFn)] = &[
    // AudioSession
    ("AudioSessionInitialize", audio_ok),
    ("AudioSessionSetActive", audio_ok),
    ("AudioSessionSetProperty", audio_ok),
    ("AudioSessionGetProperty", audio_get_property),
    ("AudioSessionAddPropertyListener", audio_ok),
    // AudioFile
    ("AudioFileOpenURL", audio_file_open),
    ("AudioFileClose", audio_ok),
    ("AudioFileGetPropertyInfo", audio_get_property),
    ("AudioFileGetProperty", audio_get_property),
    ("AudioFileReadBytes", audio_read_bytes),
    ("AudioFileReadPackets", audio_read_bytes),
    // AudioQueue
    ("AudioQueueNewOutput", audio_new_queue),
    ("AudioQueueDispose", audio_ok),
    ("AudioQueueAllocateBufferWithPacketDescriptions", audio_allocate_buffer),
    ("AudioQueueEnqueueBuffer", audio_enqueue_buffer),
    ("AudioQueueFreeBuffer", audio_ok),
    ("AudioQueueStart", audio_ok),
    ("AudioQueueStop", audio_ok),
    ("AudioQueuePause", audio_ok),
    ("AudioQueueFlush", audio_ok),
    ("AudioQueuePrime", audio_ok),
    ("AudioQueueSetParameter", audio_ok),
    ("AudioQueueGetParameter", audio_get_property),
    ("AudioQueueGetProperty", audio_get_property),
    ("AudioQueueSetProperty", audio_ok),
    ("AudioQueueGetCurrentTime", audio_get_property),
    ("AudioQueueCreateTimeline", audio_ok),
    ("AudioQueueDisposeTimeline", audio_ok),
    ("AudioQueueAddPropertyListener", audio_ok),
    ("AudioQueueRemovePropertyListener", audio_ok),
    // OpenAL
    ("alcOpenDevice", al_open_device),
    ("alcCloseDevice", al_ok_true),
    ("alcCreateContext", al_create_context),
    ("alcDestroyContext", al_ok),
    ("alcMakeContextCurrent", al_ok_true),
    ("alcGetCurrentContext", al_current_context),
    ("alcProcessContext", al_ok),
    ("alcSuspendContext", al_ok),
    ("alGenSources", al_gen_sources),
    ("alDeleteSources", al_ok),
    ("alGenBuffers", al_gen_buffers),
    ("alDeleteBuffers", al_ok),
    ("alGetError", al_no_error),
    ("alGetProcAddress", al_proc_address),
    ("alSourcePlay", al_ok),
    ("alSourceStop", al_ok),
    ("alSourcePause", al_ok),
    ("alSourceRewind", al_ok),
    ("alSourcei", al_ok),
    ("alSourcef", al_ok),
    ("alSourcefv", al_ok),
    ("alGetSourcei", al_get_source_i),
    ("alGetSourcef", al_get_source_f),
];

const AUDIO_HANDLE_BASE: u32 = 0x7a00_0000;

fn audio_ok(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn al_ok(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn al_ok_true(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(1)
}

fn al_no_error(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn al_current_context(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.sys.audio_context)
}

fn al_proc_address(hle: &mut Hle<'_>) -> Result<u32> {
    // A name pointer has no meaning for us; returning 0 tells the engine to use
    // the imported entry points instead of an extension.
    let _ = hle.arg(0);
    Ok(0)
}

fn next_handle(hle: &mut Hle<'_>) -> u32 {
    hle.sys.next_handle += 1;
    AUDIO_HANDLE_BASE + hle.sys.next_handle * 4
}

fn write_out(hle: &mut Hle<'_>, out: u32, value: u32) -> Result<()> {
    if out != 0 {
        hle.mem.write_u32(out, value)?;
    }
    Ok(())
}

fn audio_file_open(hle: &mut Hle<'_>) -> Result<u32> {
    let out = hle.arg(3);
    let handle = next_handle(hle);
    write_out(hle, out, handle)?;
    Ok(0)
}

fn audio_get_property(hle: &mut Hle<'_>) -> Result<u32> {
    // Property queries are answered with silence in the requested buffer.
    let out = hle.arg(2);
    if out != 0 {
        if let Ok(len) = hle.mem.read_u32(out) {
            if len > 0 && len < 1024 {
                let zeros = vec![0u8; len as usize];
                hle.write_bytes(out, &zeros)?;
            }
        }
    }
    Ok(0)
}

fn audio_read_bytes(hle: &mut Hle<'_>) -> Result<u32> {
    // `AudioFileReadBytes(file, useCache, position, numBytes, outNumBytes, outBuffer)`
    let out = hle.arg(4);
    write_out(hle, out, 0)?;
    Ok(0)
}

fn audio_new_queue(hle: &mut Hle<'_>) -> Result<u32> {
    let out = hle.arg(0);
    let handle = next_handle(hle);
    write_out(hle, out, handle)?;
    hle.sys.audio_queues += 1;
    Ok(0)
}

/// `AudioQueueAllocateBuffer(queue, bufferByteSize, AudioQueueBufferRef *out)`
///
/// The engine writes its mixed PCM into the returned buffer, so it must be real
/// guest memory of the requested size.
fn audio_allocate_buffer(hle: &mut Hle<'_>) -> Result<u32> {
    let size = hle.arg(1);
    let out = hle.arg(2);
    let buffer = hle.alloc(size.max(16) + 64, 16)?;
    let zeros = vec![0u8; (size.max(16) + 64) as usize];
    hle.write_bytes(buffer, &zeros)?;
    // `AudioQueueBuffer` starts with `UInt32 mAudioDataBytesCapacity; UInt8 *mAudioData;`
    hle.mem.write_u32(buffer, size)?;
    hle.mem.write_u32(buffer + 4, buffer + 64)?;
    write_out(hle, out, buffer)?;
    Ok(0)
}

fn audio_enqueue_buffer(hle: &mut Hle<'_>) -> Result<u32> {
    hle.sys.audio_buffers_queued += 1;
    Ok(0)
}

fn al_open_device(hle: &mut Hle<'_>) -> Result<u32> {
    let _ = hle.arg(0);
    Ok(next_handle(hle))
}

fn al_create_context(hle: &mut Hle<'_>) -> Result<u32> {
    let context = next_handle(hle);
    hle.sys.audio_context = context;
    hle.sys.audio_sample_rate = 44100;
    Ok(context)
}

fn al_gen_sources(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let out = hle.arg(1);
    for i in 0..count {
        let handle = next_handle(hle);
        write_out(hle, out + i * 4, handle)?;
    }
    hle.sys.openal_sources += count as u64;
    Ok(0)
}

fn al_gen_buffers(hle: &mut Hle<'_>) -> Result<u32> {
    let count = hle.arg(0);
    let out = hle.arg(1);
    for i in 0..count {
        let handle = next_handle(hle);
        write_out(hle, out + i * 4, handle)?;
    }
    hle.sys.openal_buffers += count as u64;
    Ok(0)
}

fn al_get_source_i(hle: &mut Hle<'_>) -> Result<u32> {
    // `alGetSourcei(source, param, ALint *value)`: report a playing source.
    let out = hle.arg(2);
    let value: u32 = match hle.arg(1) {
        0x1010 => 0,      // AL_SOURCE_STATE -> AL_PLAYING(0x1012)
        0x1012 => 0x1012, // AL_PLAYING
        0x1009 => 0,      // AL_BUFFERS_QUEUED
        _ => 0,
    };
    write_out(hle, out, value)?;
    Ok(0)
}

fn al_get_source_f(hle: &mut Hle<'_>) -> Result<u32> {
    let out = hle.arg(2);
    if out != 0 {
        hle.mem.write_u32(out, 1.0f32.to_bits())?;
    }
    Ok(0)
}
