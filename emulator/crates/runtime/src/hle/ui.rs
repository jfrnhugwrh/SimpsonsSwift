//! UIKit, Foundation and CoreGraphics, at the level the game touches them.
//!
//! There is no window system here: the emulator's job is to let the engine's
//! Objective-C glue (`RuntimeAppDelegate`, `EAGLView`, `CocoaTimer`, …) run to
//! completion so the C++ engine underneath it reaches its own main loop.  UIKit
//! objects therefore behave like well-formed but inert objects: they exist, they
//! answer their accessors, and the messages the engine sends to them are
//! harmless.
//!
//! The classes are exposed through the Objective-C bridge as *host classes*
//! (`objc::host_class`), so `[UIApplication sharedApplication]` and friends
//! resolve even though no library defines them.

use super::objc::HostMethod;
use super::{Hle, HleFn};
use crate::error::Result;

pub const FUNCTIONS: &[(&str, HleFn)] = &[
    ("UIApplicationMain", ui_application_main),
    ("UIGraphicsPushContext", noop_zero),
    ("UIGraphicsPopContext", noop),
    ("CGColorSpaceCreateDeviceRGB", cg_color_space),
    ("CGColorCreateGenericRGB", cg_color_create),
    ("CGBitmapContextCreate", cg_bitmap_context_create),
    ("CGContextRelease", cg_release),
    ("CGContextRetain", cg_retain),
    ("CGImageRelease", cg_release),
    ("CGImageRetain", cg_retain),
    ("CGFontRelease", cg_release),
    ("CGFontRetain", cg_retain),
    ("CGDataProviderRelease", cg_release),
    ("CGDataProviderCreateSequential", cg_object),
    ("CGImageCreateWithPNGDataProvider", cg_object),
    ("CGImageCreateWithImageInRect", cg_object),
    ("CGBitmapContextCreateImage", cg_object),
    ("CGFontCreateWithDataProvider", cg_object),
    ("CGColorSpaceRelease", cg_release),
    ("CGContextSetRGBFillColor", cg_ignore),
    ("CGContextSetRGBStrokeColor", cg_ignore),
    ("CGContextSetAllowsAntialiasing", cg_ignore),
    ("CGContextSetInterpolationQuality", cg_ignore),
    ("CGContextSetBlendMode", cg_ignore),
    ("CGContextSetShadowWithColor", cg_ignore),
    ("CGContextSetTextDrawingMode", cg_ignore),
    ("CGContextSetTextMatrix", cg_ignore),
    ("CGContextSetFont", cg_ignore),
    ("CGContextSetFontSize", cg_ignore),
    ("CGContextShowGlyphsAtPoint", cg_ignore),
    ("CGFontGetGlyphAdvances", cg_ignore),
    ("NSLog", nslog),
];

fn noop(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn noop_zero(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn cg_ignore(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn cg_object(hle: &mut Hle<'_>) -> Result<u32> {
    let handle = hle.alloc(32, 8)?;
    Ok(handle)
}

fn cg_release(_hle: &mut Hle<'_>) -> Result<u32> {
    Ok(0)
}

fn cg_retain(hle: &mut Hle<'_>) -> Result<u32> {
    Ok(hle.arg(0))
}

fn cg_color_space(hle: &mut Hle<'_>) -> Result<u32> {
    cg_object(hle)
}

fn cg_color_create(hle: &mut Hle<'_>) -> Result<u32> {
    cg_object(hle)
}

fn cg_bitmap_context_create(hle: &mut Hle<'_>) -> Result<u32> {
    // `CGBitmapContextCreate(data, width, height, bits, bytesPerRow, space, info)`
    let width = hle.arg(1);
    let height = hle.arg(2);
    hle.note(format!("CGBitmapContextCreate({width}x{height})"));
    cg_object(hle)
}

/// `int UIApplicationMain(int argc, char *argv[], NSString *principal, NSString *delegate)`
///
/// A real UIKit never returns from this call: it runs the run loop.  The
/// emulator registers the delegate class so that the runtime can deliver
/// `applicationDidFinishLaunching:` itself, and returns 0 — the machine then
/// keeps executing whatever follows (the engine's own loop or `exit`).
fn ui_application_main(hle: &mut Hle<'_>) -> Result<u32> {
    let delegate = if hle.arg(3) != 0 {
        let class = hle.arg(3);
        // The delegate is a class name string here; keep it for the runtime.
        let text = hle
            .sys
            .cf_strings
            .get(&class)
            .cloned()
            .or_else(|| hle.cstr(class).ok())
            .unwrap_or_default();
        hle.sys.app_delegate_class = text.clone();
        hle.note(format!("UIApplicationMain: delegate = {text}"));
        text
    } else {
        String::new()
    };
    let _ = delegate;
    Ok(0)
}

fn nslog(hle: &mut Hle<'_>) -> Result<u32> {
    let fmt = hle.cstr(hle.arg(0))?;
    let sp = hle.sp();
    let text = {
        let mut va = super::VaList::new(&*hle.cpu, &*hle.mem, sp, 1);
        super::format_string(&fmt, &mut va)?
    };
    hle.sys.stdout.extend_from_slice(text.as_bytes());
    hle.sys.stdout.push(b'\n');
    Ok(0)
}

// ---------------------------------------------------------------------------
// Host classes: UIKit/Foundation objects the engine talks to
// ---------------------------------------------------------------------------

#[allow(dead_code)] // available to the method tables as the identity function
fn self_(_hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(receiver)
}

fn zero(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(0)
}

fn one(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(1)
}

/// `-[UIView initWithFrame:]` and friends: keep the object, ignore geometry.
fn init_self(_hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(receiver)
}

fn view_bounds(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    // A `CGRect` return value is passed indirectly: the caller supplies a
    // buffer address in r0 (stret) and the receiver shifts by one.
    let buffer = hle.arg(0);
    write_cg_rect(hle, buffer, 0.0, 0.0, hle.sys.window_width as f32, hle.sys.window_height as f32)?;
    Ok(buffer)
}

fn screen_bounds(hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    view_bounds(hle, receiver)
}

fn write_cg_rect(hle: &mut Hle<'_>, addr: u32, x: f32, y: f32, width: f32, height: f32) -> Result<()> {
    if addr == 0 {
        return Ok(());
    }
    for (i, value) in [x, y, width, height].iter().enumerate() {
        hle.mem.write_u32(addr + (i as u32) * 4, value.to_bits())?;
    }
    Ok(())
}

fn uiscreen_main(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    super::objc::host_instance(hle, "UIScreen", 64)
}

fn uiapplication_shared(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    super::objc::host_instance(hle, "UIApplication", 64)
}

fn uidevice_current(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    super::objc::host_instance(hle, "UIDevice", 64)
}

fn nsbundle_main(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    super::objc::host_instance(hle, "NSBundle", 64)
}

fn nsdate_date(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    // Forward-declared; the instance is created by the caller through `alloc`.
    Ok(0)
}

fn nsdate_time_interval(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    let seconds = 1_300_000_000.0 + (hle.sys.nanoseconds as f64) / 1e9;
    Ok((seconds as f32).to_bits())
}

fn uitimer_scheduled(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(0)
}

fn color_clear(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(0)
}

fn bundle_path(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    let path = hle.sys.bundle_path.clone();
    crate::hle::write_guest_cstring(hle, &path)
}

fn empty_string(hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    crate::hle::write_guest_cstring(hle, "")
}

pub const HOST_METHODS: &[(&str, &str, HostMethod)] = &[
    // --- object life cycle -------------------------------------------------
    ("UIApplication", "sharedApplication", uiapplication_shared),
    ("UIScreen", "mainScreen", uiscreen_main),
    ("UIScreen", "bounds", screen_bounds),
    ("UIScreen", "applicationFrame", screen_bounds),
    ("UIScreen", "scale", one),
    ("UIDevice", "currentDevice", uidevice_current),
    ("UIDevice", "systemVersion", empty_string),
    ("UIDevice", "model", empty_string),
    ("UIDevice", "uniqueIdentifier", empty_string),
    ("NSBundle", "mainBundle", nsbundle_main),
    ("NSBundle", "bundlePath", bundle_path),
    ("NSBundle", "resourcePath", bundle_path),
    ("NSBundle", "pathForResource:ofType:", host_zero),
    // --- geometry ----------------------------------------------------------
    ("UIView", "initWithFrame:", init_self),
    ("UIView", "initWithCoder:", init_self),
    ("UIView", "bounds", view_bounds),
    ("UIView", "frame", view_bounds),
    ("UIView", "setFrame:", zero),
    ("UIView", "setBounds:", zero),
    ("UIView", "setCenter:", zero),
    ("UIView", "center", view_bounds),
    ("UIView", "addSubview:", zero),
    ("UIView", "removeFromSuperview", zero),
    ("UIView", "setNeedsDisplay", zero),
    ("UIView", "setHidden:", zero),
    ("UIView", "setAutoresizingMask:", zero),
    ("UIView", "setMultipleTouchEnabled:", zero),
    ("UIView", "setUserInteractionEnabled:", zero),
    ("UIView", "layer", host_self),
    ("UIView", "window", host_zero),
    ("UIWindow", "makeKeyAndVisible", zero),
    ("UIWindow", "initWithFrame:", init_self),
    ("UIWindow", "setRootViewController:", zero),
    ("UIApplication", "setStatusBarHidden:", zero),
    ("UIApplication", "setStatusBarStyle:", zero),
    ("UIApplication", "setStatusBarOrientation:", zero),
    ("UIApplication", "setIdleTimerDisabled:", zero),
    ("UIApplication", "setApplicationIconBadgeNumber:", zero),
    ("UIApplication", "keyWindow", host_zero),
    ("UIApplication", "openURL:", one),
    ("UIApplication", "canOpenURL:", one),
    ("UIApplication", "statusBarOrientation", zero),
    ("UIApplication", "applicationFrame", screen_bounds),
    // --- dates and timers --------------------------------------------------
    ("NSDate", "date", nsdate_date),
    ("NSDate", "timeIntervalSince1970", nsdate_time_interval),
    ("NSDate", "timeIntervalSinceReferenceDate", nsdate_time_interval),
    ("NSDate", "description", empty_string),
    ("NSTimer", "scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:", uitimer_scheduled),
    ("NSTimer", "timerWithTimeInterval:target:selector:userInfo:repeats:", uitimer_scheduled),
    ("NSTimer", "invalidate", zero),
    ("NSTimer", "isValid", one),
    ("UIColor", "clearColor", color_clear),
    ("UIColor", "blackColor", color_clear),
    ("UIColor", "whiteColor", color_clear),
    ("UIColor", "colorWithRed:green:blue:alpha:", color_clear),
    // --- strings -----------------------------------------------------------
    ("NSString", "stringWithUTF8String:", host_zero),
    ("NSString", "stringWithFormat:", host_zero),
    ("NSString", "stringByAppendingPathComponent:", host_zero),
    ("NSString", "UTF8String", host_zero),
    ("NSString", "length", one),
    ("NSString", "intValue", zero),
    ("NSString", "floatValue", zero),
    ("NSString", "doubleValue", zero),
    ("NSString", "characterAtIndex:", zero),
    ("NSString", "boolValue", zero),
    // --- collections -------------------------------------------------------
    ("NSArray", "arrayWithObjects:", host_zero),
    ("NSArray", "count", zero),
    ("NSArray", "objectAtIndex:", host_zero),
    ("NSMutableArray", "addObject:", zero),
    ("NSDictionary", "dictionaryWithObjectsAndKeys:", host_zero),
    ("NSDictionary", "objectForKey:", host_zero),
    ("NSMutableDictionary", "setObject:forKey:", zero),
    ("NSAutoreleasePool", "drain", zero),
    ("NSAutoreleasePool", "release", zero),
    // --- audio session (also stubbed by AudioToolbox) -----------------------
    ("AVAudioPlayer", "initWithContentsOfURL:error:", init_self),
    ("AVAudioPlayer", "play", one),
    ("AVAudioPlayer", "stop", zero),
    ("AVAudioPlayer", "pause", zero),
    ("AVAudioPlayer", "setNumberOfLoops:", zero),
    ("AVAudioPlayer", "setVolume:", zero),
    ("AVAudioPlayer", "setDelegate:", zero),
    ("AVAudioSession", "sharedInstance", host_zero),
    ("AVAudioSession", "setCategory:error:", zero),
    ("AVAudioSession", "setActive:error:", zero),
    ("NSProcessInfo", "processInfo", host_zero),
    ("NSProcessInfo", "arguments", host_zero),
    ("NSProcessInfo", "environment", host_zero),
    ("NSProcessInfo", "physicalMemory", host_zero),
    ("NSFileManager", "defaultManager", host_zero),
    ("NSFileManager", "fileExistsAtPath:", one),
    ("NSFileManager", "createDirectoryAtPath:withIntermediateDirectories:attributes:error:", one),
    ("NSUserDefaults", "standardUserDefaults", host_zero),
    ("NSUserDefaults", "setBool:forKey:", zero),
    ("NSUserDefaults", "boolForKey:", zero),
    ("NSUserDefaults", "setInteger:forKey:", zero),
    ("NSUserDefaults", "integerForKey:", zero),
    ("NSUserDefaults", "setObject:forKey:", zero),
    ("NSUserDefaults", "objectForKey:", host_zero),
    ("NSUserDefaults", "synchronize", one),
    ("NSNotificationCenter", "defaultCenter", host_zero),
    ("NSNotificationCenter", "addObserver:selector:name:object:", zero),
    ("NSNotificationCenter", "removeObserver:", zero),
    ("NSNotificationCenter", "postNotificationName:object:", zero),
    // --- views the engine subclasses ---------------------------------------
    ("EAGLView", "initWithFrame:", init_self),
    ("EAGLView", "setAnimationInterval:", zero),
    ("EAGLView", "setAnimationFrameInterval:", zero),
    ("EAGLView", "startAnimation", zero),
    ("EAGLView", "stopAnimation", zero),
    ("EAGLView", "setContext:", zero),
    ("EAGLView", "setFramebuffer:", zero),
    ("EAGLView", "drawView", zero),
    ("EAGLView", "swapBuffers", zero),
    ("EAGLView", "layoutSubviews", zero),
    ("EAGLView", "touchesBegan:withEvent:", zero),
    ("EAGLView", "touchesMoved:withEvent:", zero),
    ("EAGLView", "touchesEnded:withEvent:", zero),
    ("UIViewController", "viewDidLoad", zero),
    ("UIViewController", "shouldAutorotateToInterfaceOrientation:", one),
    ("UIViewController", "didReceiveMemoryWarning", zero),
    ("UIAccelerometer", "sharedAccelerometer", host_zero),
    ("UIAccelerometer", "setUpdateInterval:", zero),
    ("UIAccelerometer", "setDelegate:", zero),
    ("UIScreen", "setBrightness:", zero),
    ("MPMoviePlayerController", "initWithContentURL:", init_self),
    ("MPMoviePlayerController", "play", zero),
    ("MPMoviePlayerController", "stop", zero),
    ("MPMoviePlayerController", "setFullscreen:", zero),
    ("MPMoviePlayerController", "setControlStyle:", zero),
    ("MPMoviePlayerController", "view", host_zero),
    ("MFMailComposeViewController", "canSendMail", one),
    ("SKStoreProductViewController", "loadProductWithParameters:completionBlock:", zero),
];

fn host_self(_hle: &mut Hle<'_>, receiver: u32) -> Result<u32> {
    Ok(receiver)
}

fn host_zero(_hle: &mut Hle<'_>, _receiver: u32) -> Result<u32> {
    Ok(0)
}
