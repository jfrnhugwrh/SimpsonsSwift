# Porting `simpsons3_iphone_en` to arm64 with OpenGL ES

> **Scope note.** This is a capability test, not a real project. Every claim in
> sections 1–2 is taken from Ghidra output (program info, the import table, and
> the decompiled entry stub). Anything that goes beyond that is labelled
> **[Inference]** (a conclusion drawn from the evidence) or **[Design]** (a
> recommendation, not an observation). Section 11 lists what was *not*
> analysed.

---

## 1. Source-of-truth facts (from Ghidra)

| Property | Value |
|---|---|
| Program name | `simpsons3_iphone_en` |
| Format | Mac OS X Mach-O |
| Language ID | `ARM:LE:32:v6` (ARMv6, 32-bit, little-endian) |
| Pointer size | 4 bytes |
| Image base | `0x00001000` |
| Address range | `0x00001000` – `0x001de003` |
| Memory size | 1,953,284 bytes (~1.9 MB) |
| Functions | 5,594 (492 external) |
| Imports | 569 symbols |
| Entry (`entry`) | `0x00004c98` |
| SHA-256 | `6d1624f17bd1338d011fa2904694365c59aa2fb756c4845767462ae1ee04851c` |

### 1.1 Entry stub (`0x00004c98`)

The decompiled `entry` walks past `argv` to the end of `envp` (loops until a
zero word) and then makes two indirect calls through pointers at
`DAT_00004cdc + 0x4cc8` and `DAT_00004ce0 + 0x4cd8`. Ghidra could not recover
the jump table at `0x00004cd8`.

**[Inference]** This is the standard dyld-era `start` stub: it computes `envp`/
`apple` and jumps to `main`. Real application logic begins at the target of the
indirect call, which was not resolved. `main` is expected to call
`_UIApplicationMain` (imported), which hands control to the Objective-C app
delegate.

### 1.2 What the import table says about the architecture

| Evidence (imports) | Meaning |
|---|---|
| `___gxx_personality_sj0`, `__Unwind_SjLj_Register/Resume/Unregister`, `___cxa_throw/begin_catch/…` | C++ with **setjmp/longjmp exceptions** (the only EH model on 32-bit iOS ARM) |
| `std::string`, `ostream`, `std::cout`, `runtime_error`, `_List_node_base::hook/unhook`, `operator new/delete` | **C++ with libstdc++** (`std::list`, `std::string`, iostream) |
| `_OBJC_CLASS_$_*`, `_objc_msgSend`, `_objc_msgSend_stret`, `_objc_msgSendSuper2`, `_objc_getProperty/setProperty` | **Objective-C 2.0** (modern runtime), properties used |
| `___addsf3vfp`, `___mulsf3vfp`, `___divsf3vfp`, `___floatsisfvfp`, … | Float math compiled with **VFP** soft-helpers (ARMv6, no NEON) |
| `___divdi3`, `___floatdidf`, `___fixdfdi`, `___udivsi3`, `___modsi3`, … | 64-bit and integer division done via compiler-rt helpers (no hardware divide) |
| `glMatrixMode`, `glLoadMatrixf`, `glOrthof`, `glPushMatrix`, `glFog*`, `glAlphaFunc`, `glTexEnv*`, `glShadeModel`, `glColor4*`, `glVertexPointer`, `glColorPointer`, `glTexCoordPointer`, `glEnableClientState` | **OpenGL ES 1.1 fixed-function** pipeline |
| `glCurrentPaletteMatrixOES`, `glMatrixIndexPointerOES`, `glWeightPointerOES` | `GL_OES_matrix_palette` — **hardware skinning** |
| `gl*FramebufferOES`, `gl*RenderbufferOES`, `glFramebufferTexture2DOES`, `glCheckFramebufferStatusOES` | `GL_OES_framebuffer_object` — onscreen renderbuffer + **render-to-texture** |
| `glCompressedTexImage2D`, `glCompressedTexSubImage2D` | Compressed textures in use |
| `glClearColorx`, `glColor4x`, `glFogx`, `glTexEnvx`, `glTexParameterx` | **Fixed-point (16.16)** GL entry points |
| `_alc*`, `_alGenSources`, `_alSource*`, `_alGetProcAddress` | **OpenAL** for sound effects |
| `_AudioQueue*`, `_AudioFile*`, `_AudioSession*` | **Audio Queue** streaming (music) + C Audio Session API |
| `CAEAGLLayer`, `EAGLContext`, `kEAGLColorFormatRGB565/RGBA8`, `kEAGLDrawablePropertyRetainedBacking` | GL surface hosted in a `UIView` |
| `NSTimer` (`timerWithTimeInterval…`, `scheduledTimer…`), `NSRunLoop mainRunLoop`, `kCFRunLoopCommonModes` | **Timer-driven game loop** (no `CADisplayLink` in the import list) |
| `UIAccelerometer sharedAccelerometer`, `setUpdateInterval:` | Accelerometer input |
| `MPMoviePlayerController`, `MPMoviePlayerPlaybackDidFinishNotification` | Cut-scene / intro video |
| `MPMusicPlayerController iPodMusicPlayer`, `MPMediaPickerController` | iPod library integration |
| `CGBitmapContext*`, `CGFont*`, `CGContextShowGlyphsAtPoint`, `CGImageCreateWithPNGDataProvider`, `CGContextAddCurveToPoint/FillPath/StrokePath` | **CoreGraphics rasterisation** of text/PNG/vector shapes |
| `UITextField` (keyboard type, placeholder, return key), `UILabel`, `UIFont`, `sizeWithFont:` | Native text entry and text measurement |
| `NSURLConnection`, `NSMutableURLRequest`, `setHTTPMethod:`, `setHTTPBody:`, `setValue:forHTTPHeaderField:` | HTTP networking (POST-capable) |
| `NSUserDefaults`, `NSKeyedArchiver/Unarchiver`, `NSFileManager`, `NSSearchPathForDirectoriesInDomains`, `fopen/fread/fseek/ftell/fstat` | Persistence + raw file I/O |
| `sysctl`, `sysctlbyname`, `UIDevice` (`model`, `systemVersion`, `uniqueIdentifier`, `platformType`) | Device fingerprinting / capability tiering |
| `pthread_create`, `NSLock`, `NSRecursiveLock`, `OSAtomicAdd32` | Multithreading |
| `CFLocale*`, `CFBundleCopyBundleLocalizations`, `CFLocaleCopyPreferredLanguages` | Localisation |
| `UIApplicationLaunchOptionsRemoteNotificationKey` | Remote-notification launch path |
| `setStatusBarOrientation:`, `CGAffineTransformMakeRotation`, `setTransform:` | Manual landscape rotation |

### 1.3 Notable *absences* in the import table

Absences are as useful as presence for scoping the port:

* **No** `glGenBuffers` / `glBindBuffer` / `glBufferData` → geometry is submitted
  from **client-side arrays**, no VBOs. **[Inference]**
* **No** `glLight*`, `glMaterial*`, `glNormalPointer` → **no fixed-function
  lighting**; no normals needed in the shim. **[Inference]**
* **No** `glGetString` → no runtime extension string check; features are
  presumably gated by `UIDevice`/`sysctl` data instead. **[Inference]**
* **No** `glReadPixels` → no screenshot/readback path via GL.
* **No** stencil functions, **no** `glDepthRange`, **no** `glGenerateMipmap`.
* **No** `CADisplayLink` → frame pacing is a plain timer. **[Inference]**
* **No** `alBufferData` / `alSourceQueueBuffers` as direct imports, but
  `alGetProcAddress` **is** imported. **[Inference]** buffers are likely filled
  through an extension entry point (on iOS: `alBufferDataStatic`). Needs
  confirmation by finding the string passed to `alGetProcAddress`.

---

## 2. Why this binary cannot simply be "recompiled"

Nothing here is source code. The three realistic strategies:

| Strategy | Description | Effort | Fidelity | Notes |
|---|---|---|---|---|
| **A. Rebuild from source** | Use original source, retarget build to arm64 | Low–Medium | Exact | Not available from this analysis |
| **B. Re-implementation from decompilation** | Use Ghidra output to rebuild module-by-module in C++/ObjC | High | Good | 5,594 functions is tractable; the engine seems small |
| **C. Binary translation + shim OS** | Statically recompile ARMv6 → AArch64 and provide shim libs (GL, AL, ObjC, CF) | Very High | Exact, brittle | The ObjC runtime and SjLj EH make this the hardest route |

**[Design]** This document assumes **Strategy B**, because the app's
platform surface (OpenGL ES 1.1, OpenAL, UIKit) is wide but *shallow* and maps
neatly onto a platform abstraction layer (PAL). Sections 4–9 describe that PAL.

---

## 3. Target definition

"arm64" and "OpenGL ES" admit two main targets. This guide treats iOS as the
primary and notes Android deltas.

| | iOS arm64 | Android arm64-v8a |
|---|---|---|
| ABI | AAPCS64 (Apple variant) | AAPCS64 |
| GL | OpenGL ES 3.0 (deprecated by Apple but available) or ANGLE→Metal | EGL + OpenGL ES 3.0 |
| Audio | AVAudioEngine / OpenAL | OpenAL Soft, AAudio, or Oboe |
| UI shell | UIKit / `GLKView` or `CAEAGLLayer` | `GLSurfaceView` / `NativeActivity` |
| Compressed textures | PVRTC works on Apple GPUs | PVRTC mostly absent → ASTC/ETC2 |

**Why GLES 3.0 and not 1.1?** Core ES 1.1 is fixed-function; modern GPU
drivers and ANGLE do not offer it reliably. The recommended approach is to keep
the game's rendering call sites intact and implement a **fixed-function
emulation layer** over ES 3.0 shaders (section 5).

---

## 4. Architecture of the port

```
 ┌───────────────────────────────────────────────────────┐
 │                  Game logic (C++)                     │
 │   (unchanged semantics; fixed 64-bit audit — §7)      │
 └─────┬───────────┬───────────┬───────────┬─────────────┘
       │           │           │           │
 ┌─────▼─────┐ ┌───▼────┐ ┌────▼─────┐ ┌───▼──────────┐
 │ GL1 shim  │ │ Audio  │ │ Input /  │ │ Platform     │
 │ (ES1→ES3) │ │ PAL    │ │ Display  │ │ services PAL │
 └─────┬─────┘ └───┬────┘ └────┬─────┘ └───┬──────────┘
       │           │           │           │
   OpenGL ES 3.0  OpenAL/AAudio  UIKit/EGL  Foundation/NDK
```

**[Design]** Build the shim with the **same function names** the binary imports
(`glLoadMatrixf`, …) so decompiled call sites compile unchanged. The shim lives
in a namespace/header (`gl1_shim.h`) that `#define`s the legacy names.

---

## 5. Graphics: ES 1.1 → ES 3.0

### 5.1 Call-by-call mapping

| ES 1.1 / OES call (imported) | ES 3.0 replacement |
|---|---|
| `glMatrixMode`, `glLoadIdentity`, `glLoadMatrixf`, `glMultMatrixf`, `glPushMatrix`, `glPopMatrix`, `glTranslatef`, `glRotatef`, `glScalef`, `glOrthof` | CPU-side matrix stacks (projection, modelview, 2 × texture) uploaded as uniforms. Use column-major `float[16]` to match GL |
| `glVertexPointer`, `glColorPointer`, `glTexCoordPointer`, `glClientActiveTexture`, `glEnableClientState`, `glDisableClientState` | Record attribute pointers in shim state; at draw time copy into a streaming VBO (see 5.4) |
| `glDrawArrays`, `glDrawElements` | Flush dirty state → bind program → upload streamed data → real draw call |
| `glColor4f`, `glColor4ub`, `glColor4x` | Store current colour; feed as constant vertex attribute (`glVertexAttrib4f`) when the colour array is disabled |
| `glAlphaFunc` + `glEnable(GL_ALPHA_TEST)` | `discard` in fragment shader with compare-func + reference uniforms |
| `glFogf`, `glFogfv`, `glFogx` | Fog uniforms; computed in fragment shader (5.3) |
| `glTexEnvi`, `glTexEnvx` | Select shader permutation per texture unit (MODULATE, REPLACE, DECAL, BLEND, ADD) |
| `glShadeModel` | `flat` interpolation qualifier permutation (only if `GL_FLAT` is used) |
| `glCurrentPaletteMatrixOES`, `glMatrixIndexPointerOES`, `glWeightPointerOES` | Palette uniform array + skinning attributes (5.2) |
| `glClearColorx`, `glColor4x`, `glFogx`, `glTexEnvx`, `glTexParameterx` | Convert 16.16 fixed → float (`v / 65536.0f`) then call the float path |
| `gl*FramebufferOES`, `gl*RenderbufferOES`, `glFramebufferTexture2DOES`, `glCheckFramebufferStatusOES`, `glRenderbufferStorageOES`, `glGetRenderbufferParameterivOES` | Core ES 3.0 equivalents without the `OES` suffix and `_OES` enums |
| `glCompressedTexImage2D`, `glCompressedTexSubImage2D` | Same call; **transcode** if the format is unsupported (5.5) |
| `glTexImage2D`, `glTexSubImage2D`, `glPixelStorei`, `glTexParameteri/f` | Same call; audit formats (5.5) |
| `glBlendFunc`, `glDepthFunc`, `glDepthMask`, `glColorMask`, `glCullFace`, `glFrontFace`, `glPolygonOffset`, `glScissor`, `glViewport`, `glClear`, `glEnable/Disable` | Unchanged — but remove legacy caps (`GL_ALPHA_TEST`, `GL_FOG`, `GL_TEXTURE_2D`, `GL_LIGHTING`, …) from `glEnable/glDisable/glIsEnabled`; the shim intercepts them |
| `glGetIntegerv`, `glGetFloatv`, `glGetError`, `glIsEnabled`, `glIsTexture` | Pass through; intercept legacy matrix queries (`GL_MODELVIEW_MATRIX`) and serve from shim state |

### 5.2 Matrix-palette skinning

`glCurrentPaletteMatrixOES(i)` followed by `glLoadMatrixf` (with
`GL_MATRIX_MODE = GL_MATRIX_PALETTE_OES`) sets palette entry *i*. The vertex
position is the weighted sum of the palette transforms:

```
p_eye = Σ  weight[k] · palette[index[k]] · position          (k = 0 … N-1)
```

Shader sketch (vertex stage, GLSL ES 3.00):

```glsl
#version 300 es
#define MAX_PALETTE 32           // iOS ES1 exposed ~9; raise freely on ES3
layout(location=0) in vec4  aPosition;
layout(location=1) in vec4  aColor;
layout(location=2) in vec2  aUV0;
layout(location=3) in vec2  aUV1;
#ifdef SKINNED
layout(location=4) in vec4  aWeights;      // from glWeightPointerOES
layout(location=5) in uvec4 aIndices;      // from glMatrixIndexPointerOES (bytes)
uniform mat4 uPalette[MAX_PALETTE];
#endif
uniform mat4 uModelView;
uniform mat4 uProjection;
uniform mat4 uTexMat0, uTexMat1;
out vec4 vColor; out vec2 vUV0, vUV1; out float vEyeDist;

void main() {
#ifdef SKINNED
    vec4 p = vec4(0.0);
    for (int k = 0; k < NUM_BONES; ++k)           // NUM_BONES from glMatrixIndexPointerOES size
        p += aWeights[k] * (uPalette[aIndices[k]] * aPosition);
    vec4 eye = p;
#else
    vec4 eye = uModelView * aPosition;
#endif
    vEyeDist = abs(eye.z);
    vColor   = aColor;
    vUV0 = (uTexMat0 * vec4(aUV0, 0.0, 1.0)).xy;
    vUV1 = (uTexMat1 * vec4(aUV1, 0.0, 1.0)).xy;
    gl_Position = uProjection * eye;
}
```

Notes:
* `glMatrixIndexPointerOES` `size` = bones per vertex; `glWeightPointerOES`
  type may be `GL_BYTE/SHORT/FIXED/FLOAT` → declare attribute as normalised or
  convert on upload. Verify per call site.
* Use `glVertexAttribIPointer` for the integer index attribute.
* Because **no normals / lights are imported**, the skinning shader only
  transforms positions. **[Inference]**

### 5.3 Fragment shader (texenv, alpha test, fog)

```glsl
#version 300 es
precision mediump float;
in vec4 vColor; in vec2 vUV0, vUV1; in float vEyeDist;
uniform sampler2D uTex0, uTex1;
uniform int   uTexEnv0, uTexEnv1;        // 0 none, 1 MODULATE, 2 REPLACE, 3 DECAL, 4 ADD, 5 BLEND
uniform int   uAlphaFunc;  uniform float uAlphaRef;   // 0 = disabled
uniform int   uFogMode;    uniform vec4  uFogColor;   // 0 off, 1 LINEAR, 2 EXP, 3 EXP2
uniform float uFogStart, uFogEnd, uFogDensity;
out vec4 fragColor;

vec4 texenv(int mode, vec4 prev, vec4 t) {
    if (mode == 1) return prev * t;                        // MODULATE
    if (mode == 2) return t;                               // REPLACE
    if (mode == 3) return vec4(mix(prev.rgb, t.rgb, t.a), prev.a); // DECAL
    if (mode == 4) return vec4(prev.rgb + t.rgb, prev.a * t.a);    // ADD
    return prev;
}
void main() {
    vec4 c = vColor;
    if (uTexEnv0 != 0) c = texenv(uTexEnv0, c, texture(uTex0, vUV0));
    if (uTexEnv1 != 0) c = texenv(uTexEnv1, c, texture(uTex1, vUV1));

    if (uAlphaFunc != 0) {                                 // GL_GREATER shown; extend for all 8 funcs
        if (!(c.a > uAlphaRef)) discard;
    }
    if (uFogMode != 0) {
        float f = (uFogMode == 1) ? (uFogEnd - vEyeDist) / (uFogEnd - uFogStart)
                : (uFogMode == 2) ? exp(-uFogDensity * vEyeDist)
                                  : exp(-pow(uFogDensity * vEyeDist, 2.0));
        c.rgb = mix(uFogColor.rgb, c.rgb, clamp(f, 0.0, 1.0));
    }
    fragColor = c;
}
```

**[Design]** Do **not** use `uniform if` branching in production; generate
permutations keyed by a 32-bit state hash (`texenv0|texenv1|alpha|fog|skinned|bones`)
and cache the compiled programs. The number of distinct permutations in a
simple 2D/3D game is typically small (tens).

### 5.4 Client-side arrays → streaming VBOs

Since no buffer-object calls were imported, **all geometry is client-side**.
ES 3.0 on some stacks (notably ANGLE and Apple's drivers when a VAO is bound)
does not honour client arrays. Therefore:

1. In `glDrawArrays/Elements`, compute the byte range actually referenced
   (for indexed draws: scan the index buffer for min/max).
2. Copy that range to a **ring-buffer VBO** (e.g. 4 MB, orphan with
   `glBufferData(NULL)` or `glMapBufferRange` + `GL_MAP_INVALIDATE_RANGE_BIT`).
3. Copy indices to a ring-buffer `GL_ELEMENT_ARRAY_BUFFER`.
4. Bind a single shim VAO and issue the real draw.

Batching opportunity: collapse consecutive draws sharing state into one upload.

### 5.5 Textures

* `glCompressedTexImage2D` is used, but the **format enum is not recoverable
  from the import table**. On the original platform this is almost certainly
  PVRTC. **[Inference]**
* On iOS arm64: PVRTC still works on Apple GPUs; keep assets.
* On Android arm64: transcode PVRTC → **ASTC 4×4/6×6** (or ETC2) offline, and
  map the format in the shim.
* Audit `glTexImage2D` for `GL_LUMINANCE`, `GL_ALPHA`, `GL_LUMINANCE_ALPHA`
  (still legal in ES 3.0, but unsized internal formats need matching
  `format`/`type`) and 16-bit types (`GL_UNSIGNED_SHORT_5_6_5`,
  `_4_4_4_4`, `_5_5_5_1`), which remain valid.
* `glTexParameterx` with `GL_TEXTURE_MIN_FILTER`/wrap → convert fixed to int enum.
* `glPixelStorei(GL_UNPACK_ALIGNMENT, …)` is still valid.

### 5.6 Surface and framebuffer

The original creates a `CAEAGLLayer`-backed `UIView`, an `EAGLContext`
(`initWithAPI:` imported — API 1 = ES1), a colour renderbuffer (format
RGB565 or RGBA8, selectable) with `RetainedBacking` property imported, and
uses FBOs for render-to-texture.

| Original | iOS arm64 | Android |
|---|---|---|
| `EAGLContext initWithAPI:kEAGLRenderingAPIOpenGLES1` | `…OpenGLES3` (fallback to 2) | `eglCreateContext` with `EGL_CONTEXT_CLIENT_VERSION 3` |
| Colour renderbuffer + `presentRenderbuffer:` | `GLKView` or `CAEAGLLayer` unchanged | EGL window surface + `eglSwapBuffers` |
| `kEAGLDrawablePropertyRetainedBacking` | Set `NO` unless the game depends on frame-to-frame persistence; if it does, render to an own FBO and blit | N/A (use own FBO) |
| RGB565 option | Keep as device-tier option; otherwise RGBA8 | `EGL_RED_SIZE 8` etc. |

Depth buffer: not directly observable in imports; confirm by looking at the
`glFramebufferRenderbufferOES` attachment arguments. Prefer `DEPTH24_STENCIL8`
or `DEPTH_COMPONENT16` as the original uses.

### 5.7 Display and aspect ratio

The binary reads `UIScreen mainScreen` `bounds`. It was written for 320×480
and 480×320 (landscape via a manual `CGAffineTransform` rotation).
Modern arm64 devices have different aspect ratios, notches/safe areas, and
Retina scale factors, so:

* Choose a **virtual resolution** and letterbox or extend the viewport.
* Remove `setStatusBarOrientation:` / `setTransform:` rotation; use
  `supportedInterfaceOrientations` = landscape and render natively.
* Remap touch coordinates through the same viewport transform.
* Respect `safeAreaInsets` for HUD elements.

---

## 6. Audio

| Original (imports) | Replacement |
|---|---|
| `alcOpenDevice/CreateContext/MakeContextCurrent/Suspend/Process/Destroy/Close` | Keep OpenAL via **OpenAL Soft** (Android, and optionally iOS) or move to `AVAudioEngine` on iOS |
| `alGenSources/Buffers`, `alSource{f,fv,i,Play,Pause,Stop,Rewind}`, `alGetSource{f,i}`, `alDeleteBuffers` | Unchanged API |
| `alGetProcAddress` | On OpenAL Soft use `alBufferData` directly; if the code resolves `alBufferDataStatic`, replace with `alBufferData` (it copies) **[Inference]** |
| `AudioQueueNewOutput/Enqueue/Start/Stop/Pause/Prime/Flush`, `AudioFileOpenURL/ReadPackets/ReadBytes` | Streaming music. iOS: still functional; consider `AVAudioPlayer`/`AVAudioEngine`. Android: decode with **MediaCodec** or **libvorbis/minimp3** → AAudio/Oboe callback |
| `AudioSession*` (C API) | **Removed from modern SDKs** → `AVAudioSession` (`setCategory:` `.ambient` or `.soloAmbient`, interruption notifications) |
| `MPMusicPlayerController iPodMusicPlayer`, `playbackState` | Used to detect/keep user music. Replace with `AVAudioSession.isOtherAudioPlaying` and `.ambient` category; MediaPlayer requires an Apple Music usage description |
| `MPMediaPickerController` | User-music picker; replace with `MPMediaPickerController` (still exists, privacy prompt) or drop |
| `MPMoviePlayerController` | **Removed** → `AVPlayerViewController` / `AVPlayerLayer`; completion via `AVPlayerItemDidPlayToEndTime` |

Threading: AudioQueue callbacks and the main loop share state; audit locks (§7.4).

---

## 7. 32-bit → 64-bit code audit (the core of the arm64 work)

### 7.1 Data model changes

| Type | ARMv6 (ILP32) | arm64 (LP64) | Action |
|---|---|---|---|
| `long`, `size_t`, pointers | 4 B | 8 B | Replace `long`↔`int` assumptions; use `intptr_t`, `uint32_t`, `int32_t` |
| `NSInteger`/`NSUInteger` | 4 B | 8 B | Fix `%d` → `%ld` or cast (`NSLog`, `stringWithFormat:`) |
| `CGFloat` | `float` | `double` | Audit `CGAffineTransform*` and `CG*` calls |
| `BOOL` | `signed char` | `bool` | Never compare `BOOL` to values other than YES/NO; don't use `==` on non-normalised BOOLs |
| `char` | signed | signed (Apple) | Same on iOS; on **Android arm64** it's signed too, but check any code that depended on unsigned |

### 7.2 Serialised data and asset formats

The binary uses `fopen/fread/fseek/ftell` plus `NSKeyedArchiver` and
`NSUserDefaults`. Any `fread(&struct, sizeof(struct), …)` pattern whose struct
contains pointers, `long`, or `size_t` **will break** on-disk compatibility.

Procedure:
1. Find every `fread`/`fwrite` call site (xrefs to `_fread`, `_fwrite`).
2. Inspect the struct layout at the call site.
3. Replace with fixed-width types and explicit `#pragma pack` or manual
   (de)serialisation.
4. Keep **little-endian** (both architectures are LE — no byte-swapping).

### 7.3 Objective-C ABI differences

* **`objc_msgSend_stret` does not exist on arm64.** Struct-returning messages
  (e.g. `CGRect bounds`, `CGSize sizeWithFont:`) use plain `objc_msgSend`; the
  compiler handles it. Any hand-written casts of `objc_msgSend_stret` must go.
* **Variadic calls**: Apple's arm64 ABI passes variadic arguments on the
  **stack**. Affected imports: `stringWithFormat:`, `dictionaryWithObjectsAndKeys:`,
  `NSLog`, `printf`, `sprintf`, `handleFailureInMethod:…`. Always call through
  correctly declared prototypes; never through a non-variadic function
  pointer cast.
* **Non-fragile ivars and properties** (`objc_getProperty/setProperty`) are
  fine; recompiling regenerates ivar offsets.
* `objc_enumerationMutation` → fast enumeration still supported.
* **ARC** is optional. The binary uses manual `retain/release/autorelease`
  (imports). Porting to ARC is a separate decision; mixed MRC files are fine
  with `-fno-objc-arc`.

### 7.4 C++ runtime changes

| Original | arm64 |
|---|---|
| `__gxx_personality_sj0`, `_Unwind_SjLj_*` | **Zero-cost EH**: `__gxx_personality_v0`. No source change, but remove any code that depended on SjLj behaviour (e.g. exceptions across Obj-C boundaries — see below) |
| libstdc++ (`std::string`, `std::list`, iostream) | **libc++** on iOS/NDK; fix include paths, rebuild. `ostream operator<<` imports show iostream use — fine |
| `OSAtomicAdd32` | Deprecated → `std::atomic<int32_t>::fetch_add` / `atomic_fetch_add_explicit` |
| `pthread_create` + `pthread_attr_setdetachstate` | Unchanged. Review stack size (default 512 KB for secondary threads on iOS) |
| Exceptions thrown across ObjC frames | Undefined without `-fobjc-exceptions` interop; catch at the boundary |

### 7.5 Compiler-runtime helpers disappear

`___divsi3`, `___udivsi3`, `___modsi3`, `___umodsi3`, `___divdi3`,
`___addsf3vfp`, `___muldf3vfp`, `___floatsisfvfp`, … are provided by the
**ARMv6 toolchain**. On arm64, the compiler emits `sdiv/udiv/fadd/fmul/scvtf`
natively. No action — but it means any **inline ARM assembly** (if present;
not verified) must be rewritten for AArch64, and `-mfpu=vfp`-style flags
removed.

### 7.5.1 Optional performance work

ARMv6 had no NEON. arm64 guarantees **Advanced SIMD**: matrix multiply, vertex
skinning on the CPU, and audio mixing are candidates for NEON intrinsics.
Measure first.

### 7.6 Randomness and floating point

`rand`, `arc4random`, `sinf/cosf/atan2f/acosf/sqrtf/log10f`: results are
bit-identical on neither platform vs ARMv6 VFP. If gameplay logic uses float
determinism (replays, network lockstep), pin it with fixed-point or
`-ffp-contrast=off` and verify.

---

## 8. Platform services replacement table

| Original API (imported) | Status on modern arm64 OS | Replacement |
|---|---|---|
| `UIAccelerometer` | Removed | `CMMotionManager` (`accelerometerUpdateInterval`) |
| `UIDevice uniqueIdentifier` | **Removed** (iOS 7) | `identifierForVendor`, or generate and persist a UUID in Keychain |
| `UIDevice platformType` | Likely a custom category using `sysctlbyname("hw.machine")` **[Inference]** | Rebuild device table for current models; drive quality tiers from GPU/RAM features, not model strings |
| `NSURLConnection` + `NSMutableURLRequest` | Deprecated | `NSURLSession`; **App Transport Security** requires HTTPS or exceptions |
| `UIView beginAnimations:/commitAnimations` | Discouraged | `+[UIView animateWithDuration:animations:completion:]` |
| `NSString sizeWithFont:` | Removed | `-boundingRectWithSize:options:attributes:context:` |
| `UIApplication setStatusBarHidden:animated:`, `setStatusBarStyle:`, `setStatusBarOrientation:` | Deprecated | `prefersStatusBarHidden`, view-controller based appearance, Info.plist |
| `UIApplicationLaunchOptionsRemoteNotificationKey` | Valid | Migrate to `UNUserNotificationCenter`; request authorisation |
| `UIApplication setIdleTimerDisabled:` | Valid | Keep |
| `CGContextShowGlyphsAtPoint`, `CGFont*` | Deprecated | **CoreText** (`CTFontCreateWithGraphicsFont`, `CTLineDraw`) on iOS; **FreeType** on Android |
| `CGImageCreateWithPNGDataProvider` | Valid | Keep on iOS; `stb_image`/`libpng` on Android |
| `NSTimer` game loop | Works, but not vsync-locked | **`CADisplayLink`** (iOS) / `Choreographer` (Android); use delta-time stepping |
| `UITextField` text entry | Valid | Keep; handle keyboard height / safe area |
| `NSKeyedArchiver archivedDataWithRootObject:` | Valid, insecure-coding warnings | Move to `NSSecureCoding` |
| `NSUserDefaults`, `NSFileManager`, `NSSearchPathForDirectoriesInDomains` | Valid | Keep; note the Documents directory path is container-relative and changes across reinstalls |
| `sysctl` / `sysctlbyname` | Valid | Keep for RAM/CPU; avoid fingerprint use |
| `CFLocale*`, `CFBundleCopyBundleLocalizations` | Valid | Keep; `NSLocale.preferredLanguages` is simpler |

---

## 9. Step-by-step port plan

| Phase | Goal | Exit criteria |
|---|---|---|
| **0. Recon** | Resolve `main`, app delegate, view classes, game loop timer target | Call graph from `UIApplicationMain` to per-frame update/render |
| **1. Skeleton** | New arm64 Xcode / CMake project, ES 3.0 context, black clear colour at 60 Hz | Window opens; `glGetError() == 0` each frame |
| **2. GL1 shim** | Matrix stacks, state tracking, ubershader, client-array streaming | Draw a textured quad with fog and alpha-test, matching ES1 output |
| **3. Textures** | PVRTC/format audit, loader, mipmaps | All atlases load with no `GL_INVALID_*` |
| **4. Skinning** | Matrix-palette path | Animated character renders identically to reference frames |
| **5. FBO / RTT** | Offscreen targets for any post-effects | `glCheckFramebufferStatus == COMPLETE` |
| **6. Audio** | OpenAL context + streaming music | SFX and music play, survive interruptions |
| **7. Input & UI** | Touch, accelerometer, text field, video | Menu navigation and text entry work |
| **8. 64-bit audit** | §7 fixes, data-format validation | Saved games from the ARMv6 build load correctly |
| **9. Hardening** | Frame pacing, memory warnings, backgrounding (`alcSuspendContext`, GL calls forbidden when backgrounded) | 30-minute soak with no leaks/crashes |

### 9.1 Backgrounding

iOS forbids GL calls while the app is in the background; the process is killed
if they are issued. The shim should expose `gl1_suspend()` that stops the timer
and `glFinish()`es before `applicationWillResignActive`. Pair with
`alcSuspendContext` / `AudioSession` interruptions (all imported by the
binary, so the original has handlers to examine).

---

## 10. Validation strategy

1. **Golden-frame testing.** Capture frames from the original under an ES1 trace
   (e.g. a GL call logger) and compare against the port using a perceptual
   diff (SSIM > 0.98).
2. **Shim unit tests.** Feed known matrix stack operations and compare to a
   reference ES1 implementation.
3. **Sanitisers.** AddressSanitizer + UBSan for the 64-bit audit.
4. **Save-game round trips.** Load archives written by the ARMv6 build.
5. **Instrumentation of unknowns.** Log (a) the format enum in
   `glCompressedTexImage2D`, (b) the `size`/`type` arguments of the pointer
   calls, (c) the string argument of `alGetProcAddress`, (d) the arguments of
   `glFramebufferRenderbufferOES`. These close the **[Inference]** gaps above.

---

## 11. Coverage and limitations of this analysis

What was actually examined in Ghidra:
* `get_program_info` — complete.
* `list_imports` — all 569 imports read.
* `decompile_function_by_address(0x00004c98)` — the `entry` stub only.

What was **not** examined (so treat related statements as hypotheses):
* The 5,594 internal functions, including `main` and the app delegate.
* Strings, Objective-C class/method metadata, and data sections.
* Which compressed texture format, vertex formats, or blend modes are used.
* Whether any inline ARM assembly exists.
* Networking endpoints, DRM/receipt checks, or any server dependency.
* Asset container formats on disk.

The recommended next Ghidra queries are: resolve and decompile the target of
the indirect call in `entry`; list functions that reference `_glDrawElements`,
`_glCompressedTexImage2D`, `_alGetProcAddress` and `_NSURLConnection`; and dump
strings for file extensions and URLs.

---

## 12. Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Fixed-function emulation diverges visually (fog, texenv, alpha test) | Medium | Medium | Golden-frame tests; implement exact ES1 formulas |
| Struct-based save/asset formats break on LP64 | High | High | §7.2 audit before any gameplay work |
| Client-array → VBO streaming becomes a bottleneck | Medium | Medium | Ring buffers, batch merging, `glMapBufferRange` |
| Removed frameworks (`MPMoviePlayerController`, `UIAccelerometer`, `AudioSession`) | Certain | Low | Replacement table §6/§8 |
| Apple deprecation of OpenGL ES in favour of Metal | Certain (long-term) | Medium | Keep the shim API-agnostic so a Metal or ANGLE back-end can be swapped in |
| PVRTC unsupported on Android GPUs | High on Android | Medium | Offline transcode to ASTC/ETC2 |
| Account/online features dependent on dead servers | Unknown | Unknown | Not analysed; stub networking layer |
| Intellectual property of original assets and code | Real-world | High | Out of scope for this test; relevant to any actual project |

---

## Appendix A — Imported GL entry points (complete list, grouped)

**State / matrices:** `glMatrixMode`, `glLoadIdentity`, `glLoadMatrixf`,
`glMultMatrixf`, `glPushMatrix`, `glPopMatrix`, `glTranslatef`, `glRotatef`,
`glScalef`, `glOrthof`, `glViewport`, `glScissor`

**Raster state:** `glEnable`, `glDisable`, `glIsEnabled`, `glBlendFunc`,
`glAlphaFunc`, `glDepthFunc`, `glDepthMask`, `glColorMask`, `glCullFace`,
`glFrontFace`, `glPolygonOffset`, `glShadeModel`, `glClear`, `glClearColor`,
`glClearColorx`

**Fog / colour:** `glFogf`, `glFogfv`, `glFogx`, `glColor4f`, `glColor4ub`,
`glColor4x`

**Geometry:** `glVertexPointer`, `glColorPointer`, `glTexCoordPointer`,
`glEnableClientState`, `glDisableClientState`, `glClientActiveTexture`,
`glDrawArrays`, `glDrawElements`

**Skinning (OES):** `glCurrentPaletteMatrixOES`, `glMatrixIndexPointerOES`,
`glWeightPointerOES`

**Textures:** `glGenTextures`, `glDeleteTextures`, `glBindTexture`,
`glActiveTexture`, `glTexImage2D`, `glTexSubImage2D`, `glCompressedTexImage2D`,
`glCompressedTexSubImage2D`, `glTexParameteri`, `glTexParameterf`,
`glTexParameterx`, `glTexEnvi`, `glTexEnvx`, `glPixelStorei`, `glIsTexture`

**Framebuffers (OES):** `glGenFramebuffersOES`, `glBindFramebufferOES`,
`glDeleteFramebuffersOES`, `glFramebufferTexture2DOES`,
`glFramebufferRenderbufferOES`, `glCheckFramebufferStatusOES`,
`glGenRenderbuffersOES`, `glBindRenderbufferOES`, `glDeleteRenderbuffersOES`,
`glRenderbufferStorageOES`, `glGetRenderbufferParameterivOES`

**Queries:** `glGetError`, `glGetIntegerv`, `glGetFloatv`

## Appendix B — Suggested shim file layout

```
port/
├── gl1_shim/
│   ├── gl1_state.{h,cpp}        // matrix stacks, enables, texenv, fog, alpha
│   ├── gl1_arrays.{h,cpp}       // client pointers → ring-buffer VBO
│   ├── gl1_programs.{h,cpp}     // permutation key, shader cache
│   ├── gl1_fixed.h              // 16.16 conversion helpers
│   ├── shaders/uber.vert
│   └── shaders/uber.frag
├── audio_pal/                   // OpenAL Soft / AVAudioEngine / Oboe
├── platform_pal/                // device info, storage, HTTP, motion, text
├── legacy_ios_compat/           // UIAccelerometer, MPMovie… replacements
└── tests/                       // golden frames, shim unit tests, save-game round-trip
```
