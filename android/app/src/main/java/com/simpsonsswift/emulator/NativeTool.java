package com.simpsonsswift.emulator;

import android.content.Context;

import java.io.BufferedReader;
import java.io.File;
import java.io.IOException;
import java.io.InputStreamReader;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * Everything the app needs to know about the {@code simpsons-emu} binary that
 * ships inside the APK.
 *
 * <p>The binary is packaged as {@code lib/<abi>/libsimpsons-emu.so} so that the
 * package manager unpacks it into the application's native library directory —
 * the one place Android still allows a process to exec from.  It is a normal
 * command line program, not a JNI library, and is never {@code dlopen}ed.
 */
final class NativeTool {

    static final String BINARY = "libsimpsons-emu.so";

    private NativeTool() {
    }

    static File binary(Context context) {
        return new File(context.getApplicationInfo().nativeLibraryDir, BINARY);
    }

    static boolean isAvailable(Context context) {
        File file = binary(context);
        return file.isFile() && file.canExecute();
    }

    /** Where imported games are extracted to: private app storage. */
    static File gamesDir(Context context) {
        return new File(context.getFilesDir(), "games");
    }

    /**
     * A {@link ProcessBuilder} for the binary, with the emulator pointed at the
     * app's own directories ($HOME and the XDG data dir do not exist on
     * Android, and the library path has to be writable).
     */
    static ProcessBuilder command(Context context, List<String> arguments) {
        List<String> command = new ArrayList<>();
        command.add(binary(context).getAbsolutePath());
        command.addAll(arguments);

        ProcessBuilder builder = new ProcessBuilder(command);
        builder.redirectErrorStream(true);
        builder.directory(context.getFilesDir());

        Map<String, String> environment = builder.environment();
        environment.put("HOME", context.getFilesDir().getAbsolutePath());
        environment.put("TMPDIR", context.getCacheDir().getAbsolutePath());
        environment.put("XDG_DATA_HOME", context.getFilesDir().getAbsolutePath());
        environment.put("SIMPSONS_EMU_GAMES", gamesDir(context).getAbsolutePath());
        return builder;
    }

    /** Receives one line of a command's output. */
    interface Output {
        void line(String text);
    }

    /**
     * Runs a short command ({@code import}, {@code games}, {@code info}) to
     * completion and returns its exit code.  Call it off the main thread.
     */
    static int run(Context context, List<String> arguments, Output output) throws IOException {
        Process process = command(context, arguments).start();
        BufferedReader reader = new BufferedReader(new InputStreamReader(process.getInputStream(), "UTF-8"));
        try {
            String line;
            while ((line = reader.readLine()) != null) {
                output.line(line);
            }
        } finally {
            try {
                reader.close();
            } catch (IOException ignored) {
                // The process is going away anyway.
            }
        }
        try {
            return process.waitFor();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            process.destroy();
            return -1;
        }
    }
}
