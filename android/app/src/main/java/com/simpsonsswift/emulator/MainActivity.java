package com.simpsonsswift.emulator;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.content.Intent;
import android.content.res.AssetManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.OpenableColumns;
import android.view.View;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The whole app: run the bundled emulator, show what it prints, and show its
 * framebuffer preview.
 *
 * <p>There is no Android port of the emulator itself — the APK carries the
 * ordinary {@code simpsons-emu} command line binary, cross-compiled for the
 * four Android ABIs, and this activity drives it.
 */
public class MainActivity extends Activity implements EmulatorSession.Listener {

    private static final int REQUEST_IPA = 4711;

    /** Name of the synthetic ARMv7 Mach-O that ships with the APK. */
    private static final String DEMO_ASSET = "demo-armv7";

    /** Keep the log view bounded; the emulator can be chatty. */
    private static final int LOG_LIMIT = 192 * 1024;

    private final Handler ui = new Handler(Looper.getMainLooper());
    private final ExecutorService background = Executors.newSingleThreadExecutor();
    private final StringBuilder log = new StringBuilder();

    private TextView statusView;
    private TextView logView;
    private ScrollView logScroll;
    private WebView webView;
    private Button playButton;
    private Button demoButton;
    private Button importButton;
    private Button stopButton;
    private Button toggleButton;

    private EmulatorSession session;
    private GameLibrary.Game game;
    private File demoImage;
    private boolean previewVisible;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);

        statusView = findViewById(R.id.status);
        logView = findViewById(R.id.log);
        logScroll = findViewById(R.id.log_scroll);
        webView = findViewById(R.id.web);
        playButton = findViewById(R.id.play);
        demoButton = findViewById(R.id.demo);
        importButton = findViewById(R.id.import_ipa);
        stopButton = findViewById(R.id.stop);
        toggleButton = findViewById(R.id.toggle);

        configureWebView();

        playButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                playGame();
            }
        });
        demoButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                runDemo();
            }
        });
        importButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                pickArchive();
            }
        });
        stopButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                stopSession(true);
            }
        });
        toggleButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                showPreview(!previewVisible);
            }
        });

        stopButton.setEnabled(false);
        showPreview(false);
        append("Simpsons Arcade — ARMv7 iOS emulator");
        append("");
        prepare();
    }

    @Override
    protected void onDestroy() {
        if (isFinishing()) {
            stopSession(false);
        }
        background.shutdownNow();
        super.onDestroy();
    }

    // -----------------------------------------------------------------------
    // start-up
    // -----------------------------------------------------------------------

    /** Lay out private storage, unpack the demo image, read the library. */
    private void prepare() {
        background.execute(new Runnable() {
            @Override
            public void run() {
                final boolean haveBinary = NativeTool.isAvailable(MainActivity.this);
                File games = NativeTool.gamesDir(MainActivity.this);
                if (!games.isDirectory() && !games.mkdirs()) {
                    post("[app] cannot create " + games);
                }
                File demo = null;
                try {
                    demo = unpackDemo();
                } catch (IOException missing) {
                    post("[app] no demo image in this build: " + missing.getMessage());
                }
                final File unpacked = demo;
                final List<GameLibrary.Game> library = GameLibrary.scan(games);
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        demoImage = unpacked;
                        game = library.isEmpty() ? null : library.get(0);
                        if (!haveBinary) {
                            append("[app] this APK has no emulator binary for this device's ABI (" + abi() + ").");
                            append("[app] install the universal APK from the release page.");
                            playButton.setEnabled(false);
                            demoButton.setEnabled(false);
                            importButton.setEnabled(false);
                            setStatus("no native binary for " + abi());
                            return;
                        }
                        demoButton.setEnabled(unpacked != null);
                        describeLibrary(library);
                        refreshStatus();
                    }
                });
            }
        });
    }

    private void describeLibrary(List<GameLibrary.Game> games) {
        if (games.isEmpty()) {
            append("No game imported yet.  This emulator ships no game and downloads nothing:");
            append("tap \u201cImport .ipa\u201d and pick your own decrypted copy of");
            append("The Simpsons Arcade v1.1.43, or tap \u201cDemo\u201d to boot a synthetic");
            append("ARMv7 Mach-O and watch the loader, the interpreter and the HLE work.");
        } else {
            append("Game library:");
            for (GameLibrary.Game entry : games) {
                append("  " + entry.label + "  (" + entry.bundleDir.getName() + ")");
            }
        }
        append("");
    }

    /** Copy the bundled demo image out of the APK so the emulator can read it. */
    private File unpackDemo() throws IOException {
        File destination = new File(getFilesDir(), DEMO_ASSET);
        AssetManager assets = getAssets();
        InputStream input = assets.open(DEMO_ASSET);
        OutputStream output = null;
        try {
            output = new FileOutputStream(destination);
            copy(input, output);
        } finally {
            closeQuietly(input);
            closeQuietly(output);
        }
        return destination;
    }

    // -----------------------------------------------------------------------
    // running
    // -----------------------------------------------------------------------

    private void playGame() {
        if (game == null) {
            toast("Import an .ipa first");
            showPreview(false);
            return;
        }
        List<String> arguments = new ArrayList<>(Arrays.asList(
                "run", game.executable.getAbsolutePath(),
                "--bundle", game.bundleDir.getAbsolutePath(),
                "--tolerate-undefined",
                "--max-insns", "100000000000"));
        start(arguments, game.label);
    }

    private void runDemo() {
        if (demoImage == null || !demoImage.isFile()) {
            toast("The demo image is missing from this build");
            return;
        }
        List<String> arguments = new ArrayList<>(Arrays.asList(
                "run", demoImage.getAbsolutePath(),
                "--trace",
                "--stats",
                "--verbose"));
        start(arguments, "demo (synthetic ARMv7 Mach-O)");
    }

    /** Start `simpsons-emu run ... --serve <port> --bind 127.0.0.1`. */
    private void start(List<String> arguments, final String what) {
        stopSession(false);
        final int port = EmulatorSession.freePort();
        arguments.add("--serve");
        arguments.add(Integer.toString(port));
        // Loopback only: the preview never leaves the phone.
        arguments.add("--bind");
        arguments.add("127.0.0.1");

        append("");
        append("$ simpsons-emu " + join(arguments));
        setStatus("starting " + what + "\u2026");
        showPreview(false);
        try {
            session = EmulatorSession.start(this, arguments, port, this);
            playButton.setEnabled(false);
            demoButton.setEnabled(false);
            stopButton.setEnabled(true);
        } catch (IOException failed) {
            append("[app] could not start the emulator: " + failed);
            setStatus("failed to start");
        }
    }

    private void stopSession(boolean announce) {
        EmulatorSession running = session;
        session = null;
        if (running != null && running.isRunning()) {
            running.stop();
            if (announce) {
                append("[app] stopped");
            }
        }
    }

    @Override
    public void onLine(final String text) {
        post(text);
    }

    @Override
    public void onReady(final int port) {
        ui.post(new Runnable() {
            @Override
            public void run() {
                append("[app] preview ready on 127.0.0.1:" + port);
                webView.loadUrl("http://127.0.0.1:" + port + "/");
                showPreview(true);
                setStatus("running \u2014 preview on 127.0.0.1:" + port);
            }
        });
    }

    @Override
    public void onExit(final int code) {
        ui.post(new Runnable() {
            @Override
            public void run() {
                session = null;
                playButton.setEnabled(true);
                demoButton.setEnabled(demoImage != null);
                stopButton.setEnabled(false);
                webView.loadUrl("about:blank");
                showPreview(false);
                append("[app] emulator exited with status " + code);
                refreshStatus();
            }
        });
    }

    // -----------------------------------------------------------------------
    // importing
    // -----------------------------------------------------------------------

    private void pickArchive() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        try {
            startActivityForResult(intent, REQUEST_IPA);
        } catch (RuntimeException noPicker) {
            toast("No file picker on this device");
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQUEST_IPA || resultCode != RESULT_OK || data == null || data.getData() == null) {
            return;
        }
        importArchive(data.getData(), false);
    }

    private void importArchive(final Uri uri, final boolean allowOtherApp) {
        showPreview(false);
        setStatus("importing\u2026");
        importButton.setEnabled(false);
        background.execute(new Runnable() {
            @Override
            public void run() {
                File staged = null;
                try {
                    staged = stage(uri);
                    post("");
                    post("$ simpsons-emu import " + staged.getName() + (allowOtherApp ? " --allow-other-app" : ""));
                    List<String> arguments = new ArrayList<>(Arrays.asList(
                            "import", staged.getAbsolutePath(),
                            "--dest", NativeTool.gamesDir(MainActivity.this).getAbsolutePath(),
                            "--force"));
                    if (allowOtherApp) {
                        arguments.add("--allow-other-app");
                    }
                    final StringBuilder output = new StringBuilder();
                    int code = NativeTool.run(MainActivity.this, arguments, new NativeTool.Output() {
                        @Override
                        public void line(String text) {
                            output.append(text).append('\n');
                            post(text);
                        }
                    });
                    final boolean refusedApp = code != 0 && output.indexOf("--allow-other-app") >= 0;
                    finishImport(code, refusedApp, uri);
                } catch (IOException failed) {
                    post("[app] import failed: " + failed);
                    finishImport(-1, false, uri);
                } finally {
                    if (staged != null && !staged.delete()) {
                        post("[app] note: could not delete the staged copy at " + staged);
                    }
                }
            }
        });
    }

    private void finishImport(final int code, final boolean refusedApp, final Uri uri) {
        final List<GameLibrary.Game> games = GameLibrary.scan(NativeTool.gamesDir(this));
        ui.post(new Runnable() {
            @Override
            public void run() {
                importButton.setEnabled(true);
                game = games.isEmpty() ? null : games.get(0);
                refreshStatus();
                if (code == 0) {
                    append("[app] imported \u2014 tap Play");
                    toast("Imported");
                } else if (refusedApp) {
                    askAboutOtherApp(uri);
                }
            }
        });
    }

    private void askAboutOtherApp(final Uri uri) {
        new AlertDialog.Builder(this)
                .setTitle("Not The Simpsons Arcade")
                .setMessage("That archive is a valid iOS app, but not the game this emulator targets "
                        + "(v1.1.43).  Import it anyway?  The HLE surface is written for that release, "
                        + "so anything else is unlikely to boot.")
                .setNegativeButton("Cancel", null)
                .setPositiveButton("Import anyway", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        importArchive(uri, true);
                    }
                })
                .show();
    }

    /** Copy the picked document into the cache so the CLI can open it by path. */
    private File stage(Uri uri) throws IOException {
        File directory = new File(getCacheDir(), "incoming");
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IOException("cannot create " + directory);
        }
        File destination = new File(directory, fileName(uri));
        InputStream input = getContentResolver().openInputStream(uri);
        if (input == null) {
            throw new IOException("the file picker returned nothing readable");
        }
        OutputStream output = null;
        try {
            output = new FileOutputStream(destination);
            long bytes = copy(input, output);
            post("[app] staged " + destination.getName() + " (" + bytes + " bytes)");
        } finally {
            closeQuietly(input);
            closeQuietly(output);
        }
        return destination;
    }

    private String fileName(Uri uri) {
        String name = null;
        Cursor cursor = null;
        try {
            cursor = getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null);
            if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                name = cursor.getString(0);
            }
        } catch (RuntimeException unreadable) {
            name = null;
        } finally {
            if (cursor != null) {
                cursor.close();
            }
        }
        if (name == null || name.trim().isEmpty()) {
            name = "import.ipa";
        }
        name = name.replace('/', '_').replace('\\', '_');
        return name.length() > 96 ? name.substring(name.length() - 96) : name;
    }

    // -----------------------------------------------------------------------
    // plumbing
    // -----------------------------------------------------------------------

    private void configureWebView() {
        WebSettings settings = webView.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        settings.setCacheMode(WebSettings.LOAD_NO_CACHE);
        settings.setUseWideViewPort(true);
        settings.setLoadWithOverviewMode(true);
        settings.setBuiltInZoomControls(true);
        settings.setDisplayZoomControls(false);
        webView.setBackgroundColor(0xFF10121A);
        webView.setWebViewClient(new WebViewClient());
    }

    private void showPreview(boolean preview) {
        previewVisible = preview;
        webView.setVisibility(preview ? View.VISIBLE : View.GONE);
        logScroll.setVisibility(preview ? View.GONE : View.VISIBLE);
        toggleButton.setText(preview ? R.string.log : R.string.preview);
    }

    private void refreshStatus() {
        if (session != null && session.isRunning()) {
            return;
        }
        if (game != null) {
            setStatus("ready \u2014 " + game.label);
        } else {
            setStatus("ready \u2014 no game imported");
        }
    }

    private void setStatus(String text) {
        statusView.setText(text);
    }

    /** Append a line from a background thread. */
    private void post(final String text) {
        ui.post(new Runnable() {
            @Override
            public void run() {
                append(text);
            }
        });
    }

    private void append(String text) {
        log.append(text).append('\n');
        if (log.length() > LOG_LIMIT) {
            log.delete(0, log.length() - LOG_LIMIT);
        }
        logView.setText(log.toString());
        logScroll.post(new Runnable() {
            @Override
            public void run() {
                logScroll.fullScroll(View.FOCUS_DOWN);
            }
        });
    }

    private void toast(String text) {
        Toast.makeText(this, text, Toast.LENGTH_SHORT).show();
    }

    private String abi() {
        String[] abis = android.os.Build.SUPPORTED_ABIS;
        return abis != null && abis.length > 0 ? abis[0] : "unknown";
    }

    private static String join(List<String> parts) {
        StringBuilder text = new StringBuilder();
        for (String part : parts) {
            if (text.length() > 0) {
                text.append(' ');
            }
            text.append(part.indexOf(' ') >= 0 ? "\"" + part + "\"" : part);
        }
        return text.toString();
    }

    private static long copy(InputStream input, OutputStream output) throws IOException {
        byte[] buffer = new byte[64 * 1024];
        long total = 0;
        int read;
        while ((read = input.read(buffer)) > 0) {
            output.write(buffer, 0, read);
            total += read;
        }
        output.flush();
        return total;
    }

    private static void closeQuietly(InputStream stream) {
        if (stream != null) {
            try {
                stream.close();
            } catch (IOException ignored) {
                // Nothing useful to do.
            }
        }
    }

    private static void closeQuietly(OutputStream stream) {
        if (stream != null) {
            try {
                stream.close();
            } catch (IOException ignored) {
                // Nothing useful to do.
            }
        }
    }
}
