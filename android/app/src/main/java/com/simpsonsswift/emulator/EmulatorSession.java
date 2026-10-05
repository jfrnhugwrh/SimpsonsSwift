package com.simpsonsswift.emulator;

import android.content.Context;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.util.List;

/**
 * One run of the emulator: {@code simpsons-emu run <image> --serve <port>}.
 *
 * <p>The process writes its boot trace to stdout, which is pumped into the log
 * view, and serves the live framebuffer over loopback, which is what the
 * WebView shows once the port answers.
 */
final class EmulatorSession {

    interface Listener {
        /** A line of the emulator's own output. */
        void onLine(String text);

        /** The preview server accepted a connection; the UI can load it. */
        void onReady(int port);

        /** The process finished (or was stopped). */
        void onExit(int code);
    }

    /** How long to wait for the preview server before giving up on it. */
    private static final long READY_TIMEOUT_MS = 30_000L;

    private final Process process;
    private final int port;
    private volatile boolean stopping;

    private EmulatorSession(Process process, int port) {
        this.process = process;
        this.port = port;
    }

    /** A port the emulator can almost certainly bind a moment from now. */
    static int freePort() {
        ServerSocket probe = null;
        try {
            probe = new ServerSocket(0);
            return probe.getLocalPort();
        } catch (IOException unavailable) {
            return 8723;
        } finally {
            if (probe != null) {
                try {
                    probe.close();
                } catch (IOException ignored) {
                    // Nothing to do; the port is still usable.
                }
            }
        }
    }

    static EmulatorSession start(Context context, List<String> arguments, int port, Listener listener)
            throws IOException {
        Process process = NativeTool.command(context, arguments).start();
        EmulatorSession session = new EmulatorSession(process, port);

        Thread output = new Thread(new Runnable() {
            @Override
            public void run() {
                session.pump(listener);
            }
        }, "simpsons-emu-output");
        output.setDaemon(true);
        output.start();

        Thread ready = new Thread(new Runnable() {
            @Override
            public void run() {
                session.awaitServer(listener);
            }
        }, "simpsons-emu-ready");
        ready.setDaemon(true);
        ready.start();

        return session;
    }

    int port() {
        return port;
    }

    boolean isRunning() {
        try {
            process.exitValue();
            return false;
        } catch (IllegalThreadStateException stillRunning) {
            return true;
        }
    }

    void stop() {
        stopping = true;
        process.destroy();
    }

    private void pump(Listener listener) {
        BufferedReader reader = null;
        try {
            reader = new BufferedReader(new InputStreamReader(process.getInputStream(), "UTF-8"));
            String line;
            while ((line = reader.readLine()) != null) {
                listener.onLine(line);
            }
        } catch (IOException broken) {
            if (!stopping) {
                listener.onLine("[app] reading the emulator's output failed: " + broken);
            }
        } finally {
            if (reader != null) {
                try {
                    reader.close();
                } catch (IOException ignored) {
                    // The process is gone; nothing to recover.
                }
            }
        }
        int code = -1;
        try {
            code = process.waitFor();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
        listener.onExit(code);
    }

    private void awaitServer(Listener listener) {
        long deadline = System.currentTimeMillis() + READY_TIMEOUT_MS;
        while (System.currentTimeMillis() < deadline && isRunning() && !stopping) {
            Socket socket = new Socket();
            try {
                socket.connect(new InetSocketAddress("127.0.0.1", port), 250);
                listener.onReady(port);
                return;
            } catch (IOException notYet) {
                try {
                    Thread.sleep(150L);
                } catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                    return;
                }
            } finally {
                try {
                    socket.close();
                } catch (IOException ignored) {
                    // Probe socket only.
                }
            }
        }
    }
}
