package com.simpsonsswift.emulator;

import android.app.Activity;
import android.content.Intent;
import android.content.res.ColorStateList;
import android.graphics.Color;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceRequest;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import org.json.JSONObject;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.ServerSocket;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Small Android front end for the repository's Rust CLI. The Rust executable is
 * packaged in the ABI-specific native-library directory and launched as a child
 * process; the existing local HTTP preview supplies the framebuffer UI.
 */
public final class MainActivity extends Activity {
    private static final int PICK_IPA_REQUEST = 41;
    private static final int PICK_WEB_FILE_REQUEST = 42;
    private static final long MAX_IPA_BYTES = 512L * 1024L * 1024L;
    private static final int MAX_CONSOLE_CHARS = 24_000;

    private static final int COLOR_BACKGROUND = Color.rgb(16, 18, 26);
    private static final int COLOR_PANEL = Color.rgb(25, 29, 41);
    private static final int COLOR_TEXT = Color.rgb(232, 234, 242);
    private static final int COLOR_MUTED = Color.rgb(165, 171, 190);
    private static final int COLOR_ACCENT = Color.rgb(244, 197, 66);

    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private volatile Process emulatorProcess;
    private volatile boolean stopRequested;

    private File gamesRoot;
    private Uri selectedIpaUri;
    private ImportedGame importedGame;
    private ValueCallback<Uri[]> webFileCallback;
    private boolean busy;
    private boolean startingEmulator;

    private Button chooseButton;
    private Button importButton;
    private Button refreshButton;
    private Button runButton;
    private Button stopButton;
    private TextView selectedFileText;
    private TextView libraryText;
    private TextView statusText;
    private TextView consoleText;
    private ScrollView pageScroller;
    private ScrollView consoleScroll;
    private WebView preview;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setStatusBarColor(COLOR_BACKGROUND);
        getWindow().setNavigationBarColor(COLOR_BACKGROUND);

        gamesRoot = new File(getFilesDir(), "games");
        buildUi();
        refreshLibrary();
    }

    private void buildUi() {
        pageScroller = new ScrollView(this);
        pageScroller.setFillViewport(true);
        pageScroller.setBackgroundColor(COLOR_BACKGROUND);
        if (Build.VERSION.SDK_INT >= 35) {
            // Android 15 enforces edge-to-edge for targetSdk 35 apps. Keep the
            // scrollable content clear of the system status/navigation bars.
            pageScroller.setOnApplyWindowInsetsListener((view, insets) -> {
                view.setPadding(0, insets.getSystemWindowInsetTop(), 0, insets.getSystemWindowInsetBottom());
                return insets;
            });
        }

        LinearLayout page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setPadding(dp(20), dp(22), dp(20), dp(28));
        pageScroller.addView(page, new ScrollView.LayoutParams(
                ScrollView.LayoutParams.MATCH_PARENT,
                ScrollView.LayoutParams.WRAP_CONTENT));
        setContentView(pageScroller);

        TextView brand = makeText("SIMPSONSSWIFT", 13, COLOR_ACCENT, true);
        brand.setLetterSpacing(0.12f);
        page.addView(brand, matchWrap());

        TextView title = makeText("The Simpsons Arcade", 25, COLOR_TEXT, true);
        LinearLayout.LayoutParams titleParams = matchWrap();
        titleParams.topMargin = dp(5);
        page.addView(title, titleParams);

        TextView intro = makeText(
                "This APK contains the emulator, not the game. Select a decrypted IPA you obtained yourself; it is imported into this app's private storage and is never uploaded. The game boot path is still experimental.",
                14, COLOR_MUTED, false);
        intro.setLineSpacing(dp(3), 1.0f);
        LinearLayout.LayoutParams introParams = matchWrap();
        introParams.topMargin = dp(8);
        introParams.bottomMargin = dp(18);
        page.addView(intro, introParams);

        chooseButton = makeButton("Choose decrypted IPA", true);
        chooseButton.setOnClickListener(view -> launchPicker(PICK_IPA_REQUEST));
        page.addView(chooseButton, matchWrap());

        selectedFileText = makeText("No IPA selected", 13, COLOR_MUTED, false);
        LinearLayout.LayoutParams fileParams = matchWrap();
        fileParams.topMargin = dp(7);
        fileParams.bottomMargin = dp(8);
        page.addView(selectedFileText, fileParams);

        importButton = makeButton("Import game", false);
        importButton.setOnClickListener(view -> importSelectedIpa());
        page.addView(importButton, matchWrap());

        LinearLayout libraryRow = new LinearLayout(this);
        libraryRow.setOrientation(LinearLayout.HORIZONTAL);
        libraryRow.setGravity(Gravity.CENTER_VERTICAL);
        LinearLayout.LayoutParams libraryRowParams = matchWrap();
        libraryRowParams.topMargin = dp(14);
        page.addView(libraryRow, libraryRowParams);

        TextView libraryHeader = makeText("GAME LIBRARY", 12, COLOR_ACCENT, true);
        libraryHeader.setLetterSpacing(0.08f);
        libraryRow.addView(libraryHeader, new LinearLayout.LayoutParams(0, dp(42), 1.0f));

        refreshButton = makeButton("Refresh", false);
        refreshButton.setOnClickListener(view -> refreshLibrary());
        libraryRow.addView(refreshButton, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT, dp(42)));

        libraryText = makeText("Looking for imported games…", 13, COLOR_TEXT, false);
        libraryText.setBackgroundColor(COLOR_PANEL);
        libraryText.setPadding(dp(12), dp(10), dp(12), dp(10));
        page.addView(libraryText, matchWrap());

        LinearLayout controls = new LinearLayout(this);
        controls.setOrientation(LinearLayout.HORIZONTAL);
        LinearLayout.LayoutParams controlsParams = matchWrap();
        controlsParams.topMargin = dp(12);
        page.addView(controls, controlsParams);

        runButton = makeButton("Start emulator", true);
        runButton.setOnClickListener(view -> startEmulator());
        LinearLayout.LayoutParams runParams = new LinearLayout.LayoutParams(0, dp(48), 1.0f);
        runParams.rightMargin = dp(6);
        controls.addView(runButton, runParams);

        stopButton = makeButton("Stop", false);
        stopButton.setOnClickListener(view -> stopEmulator());
        LinearLayout.LayoutParams stopParams = new LinearLayout.LayoutParams(0, dp(48), 1.0f);
        stopParams.leftMargin = dp(6);
        controls.addView(stopButton, stopParams);

        statusText = makeText("Choose a decrypted IPA to get started.", 13, COLOR_MUTED, false);
        statusText.setBackgroundColor(COLOR_PANEL);
        statusText.setPadding(dp(12), dp(10), dp(12), dp(10));
        LinearLayout.LayoutParams statusParams = matchWrap();
        statusParams.topMargin = dp(10);
        page.addView(statusText, statusParams);

        preview = new WebView(this);
        preview.setBackgroundColor(Color.BLACK);
        preview.getSettings().setJavaScriptEnabled(true);
        preview.getSettings().setDomStorageEnabled(false);
        preview.getSettings().setAllowFileAccess(false);
        preview.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                Uri uri = request.getUrl();
                String host = uri == null ? null : uri.getHost();
                return !("127.0.0.1".equals(host) || "localhost".equals(host));
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, String url) {
                Uri uri = Uri.parse(url);
                String host = uri.getHost();
                return !("127.0.0.1".equals(host) || "localhost".equals(host));
            }
        });
        preview.setWebChromeClient(new WebChromeClient() {
            @Override
            public boolean onShowFileChooser(
                    WebView webView,
                    ValueCallback<Uri[]> filePathCallback,
                    FileChooserParams fileChooserParams) {
                if (webFileCallback != null) {
                    webFileCallback.onReceiveValue(null);
                }
                webFileCallback = filePathCallback;
                if (!launchPicker(PICK_WEB_FILE_REQUEST)) {
                    webFileCallback = null;
                    filePathCallback.onReceiveValue(null);
                    return false;
                }
                return true;
            }
        });
        preview.setVisibility(View.GONE);
        LinearLayout.LayoutParams previewParams = matchWrap();
        previewParams.topMargin = dp(14);
        previewParams.height = dp(420);
        page.addView(preview, previewParams);

        TextView consoleHeader = makeText("EMULATOR OUTPUT", 12, COLOR_ACCENT, true);
        consoleHeader.setLetterSpacing(0.08f);
        LinearLayout.LayoutParams consoleHeaderParams = matchWrap();
        consoleHeaderParams.topMargin = dp(16);
        consoleHeaderParams.bottomMargin = dp(7);
        page.addView(consoleHeader, consoleHeaderParams);

        consoleScroll = new ScrollView(this);
        consoleScroll.setBackgroundColor(COLOR_PANEL);
        consoleText = makeText(
                "The emulator log and any import or startup errors will appear here.",
                12, COLOR_TEXT, false);
        consoleText.setTypeface(Typeface.MONOSPACE);
        consoleText.setTextIsSelectable(true);
        consoleText.setPadding(dp(12), dp(12), dp(12), dp(12));
        consoleScroll.addView(consoleText, new ScrollView.LayoutParams(
                ScrollView.LayoutParams.MATCH_PARENT,
                ScrollView.LayoutParams.WRAP_CONTENT));
        LinearLayout.LayoutParams consoleParams = matchWrap();
        consoleParams.height = dp(210);
        page.addView(consoleScroll, consoleParams);

        updateButtons();
    }

    private TextView makeText(String text, float sizeSp, int color, boolean bold) {
        TextView view = new TextView(this);
        view.setText(text);
        view.setTextSize(sizeSp);
        view.setTextColor(color);
        if (bold) {
            view.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        }
        return view;
    }

    private Button makeButton(String text, boolean primary) {
        Button button = new Button(this);
        button.setText(text);
        button.setAllCaps(false);
        button.setTextSize(14);
        button.setTextColor(primary ? COLOR_BACKGROUND : COLOR_TEXT);
        button.setBackgroundTintList(ColorStateList.valueOf(
                primary ? COLOR_ACCENT : Color.rgb(48, 55, 73)));
        return button;
    }

    private LinearLayout.LayoutParams matchWrap() {
        return new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private boolean launchPicker(int requestCode) {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        intent.putExtra(Intent.EXTRA_MIME_TYPES, new String[]{
                "application/octet-stream", "application/zip", "application/x-ipa"
        });
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        try {
            startActivityForResult(intent, requestCode);
            return true;
        } catch (Exception error) {
            setStatus("Could not open the Android file picker: " + error.getMessage());
            return false;
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        Uri uri = resultCode == RESULT_OK && data != null ? data.getData() : null;

        if (requestCode == PICK_WEB_FILE_REQUEST) {
            if (webFileCallback != null) {
                webFileCallback.onReceiveValue(uri == null ? null : new Uri[]{uri});
                webFileCallback = null;
            }
            return;
        }

        if (requestCode == PICK_IPA_REQUEST && uri != null) {
            try {
                getContentResolver().takePersistableUriPermission(
                        uri, Intent.FLAG_GRANT_READ_URI_PERMISSION);
            } catch (SecurityException ignored) {
                // The selected provider can grant a temporary read permission;
                // the file is copied as soon as the user taps Import.
            }
            selectedIpaUri = uri;
            String name = uri.getLastPathSegment();
            selectedFileText.setText("Selected: " + (name == null ? "IPA document" : name));
            setStatus("Ready to validate and import the selected archive.");
            updateButtons();
        }
    }

    private void importSelectedIpa() {
        final Uri uri = selectedIpaUri;
        if (uri == null || busy || isEmulatorRunning()) {
            return;
        }

        busy = true;
        setStatus("Copying and validating the IPA…");
        appendConsole("Import started. The source IPA is temporary and will be removed after import.");
        updateButtons();

        worker.execute(() -> {
            File temporaryIpa = new File(getCacheDir(), "simpsons-selected.ipa");
            ImportedGame result = null;
            String failure = null;
            try {
                if (temporaryIpa.exists() && !temporaryIpa.delete()) {
                    throw new IOException("Could not clear the previous temporary IPA copy.");
                }
                long copied = copyUriToFile(uri, temporaryIpa);
                if (copied == 0) {
                    throw new IOException("The selected file was empty.");
                }

                File binary = requireEmulatorBinary();
                Process process = new ProcessBuilder(
                        binary.getAbsolutePath(),
                        "import",
                        temporaryIpa.getAbsolutePath(),
                        "--dest",
                        gamesRoot.getAbsolutePath(),
                        "--force")
                        .directory(getFilesDir())
                        .redirectErrorStream(true)
                        .start();
                int exitCode = streamOutputAndWait(process);
                if (exitCode != 0) {
                    throw new IOException("The importer exited with code " + exitCode + ". See emulator output.");
                }

                result = findLatestGame(gamesRoot);
                if (result == null) {
                    throw new IOException("Import finished, but no usable game manifest was found.");
                }
            } catch (Exception error) {
                failure = error.getMessage() == null ? error.toString() : error.getMessage();
                appendConsole("Import error: " + failure);
            } finally {
                if (temporaryIpa.exists() && !temporaryIpa.delete()) {
                    appendConsole("Warning: could not remove the temporary IPA from the app cache.");
                }
            }

            final ImportedGame imported = result;
            final String errorMessage = failure;
            runOnUiThread(() -> {
                busy = false;
                if (imported != null) {
                    importedGame = imported;
                    selectedIpaUri = null;
                    selectedFileText.setText("IPA imported; the original archive was not retained.");
                    libraryText.setText(imported.label + "\n" + imported.executable.getParent());
                    setStatus("Imported " + imported.label + ". Start the emulator when ready.");
                } else {
                    setStatus("Import failed: " + errorMessage);
                }
                updateButtons();
            });
        });
    }

    private long copyUriToFile(Uri uri, File target) throws IOException {
        InputStream opened = getContentResolver().openInputStream(uri);
        if (opened == null) {
            throw new IOException("Android could not read the selected document.");
        }
        long total = 0;
        byte[] buffer = new byte[64 * 1024];
        try (InputStream input = new BufferedInputStream(opened);
             OutputStream output = new BufferedOutputStream(new FileOutputStream(target))) {
            int count;
            while ((count = input.read(buffer)) != -1) {
                total += count;
                if (total > MAX_IPA_BYTES) {
                    throw new IOException("IPA is larger than this app's 512 MiB import limit.");
                }
                output.write(buffer, 0, count);
            }
        }
        return total;
    }

    private void refreshLibrary() {
        if (busy || isEmulatorRunning()) {
            return;
        }
        busy = true;
        libraryText.setText("Scanning private game library…");
        updateButtons();
        worker.execute(() -> {
            ImportedGame latest = findLatestGame(gamesRoot);
            runOnUiThread(() -> {
                importedGame = latest;
                busy = false;
                if (latest == null) {
                    libraryText.setText("No imported game yet. Choose your own decrypted Simpsons Arcade IPA above.");
                    setStatus("Choose a decrypted IPA to get started.");
                } else {
                    libraryText.setText(latest.label + "\n" + latest.executable.getParent());
                    setStatus("Game ready. Start the emulator to open its live preview.");
                }
                updateButtons();
            });
        });
    }

    private ImportedGame findLatestGame(File root) {
        File[] children = root.listFiles();
        if (children == null) {
            return null;
        }

        ImportedGame latest = null;
        for (File directory : children) {
            if (!directory.isDirectory()) {
                continue;
            }
            File manifest = new File(directory, "import.json");
            if (!manifest.isFile()) {
                continue;
            }
            try {
                String jsonText = readUtf8(manifest);
                JSONObject json = new JSONObject(jsonText);
                String appBundle = safeChildName(json.optString("app_bundle", ""));
                String executableName = safeChildName(json.optString("executable", ""));
                if (appBundle == null || executableName == null) {
                    continue;
                }
                File bundle = new File(directory, appBundle);
                File executable = new File(bundle, executableName);
                String rootPath = directory.getCanonicalPath() + File.separator;
                if (!executable.getCanonicalPath().startsWith(rootPath) || !executable.isFile()) {
                    continue;
                }

                String title = json.optString("display_name", "");
                if (title.isEmpty()) {
                    title = json.optString("title", "Imported game");
                }
                String version = json.optString("version", "");
                String label = version.isEmpty() ? title : title + " " + version;
                long importedAt = json.optLong("imported_unix", directory.lastModified() / 1000L);
                ImportedGame candidate = new ImportedGame(bundle, executable, label, importedAt);
                if (latest == null || candidate.importedAt >= latest.importedAt) {
                    latest = candidate;
                }
            } catch (Exception ignored) {
                // Skip damaged or hand-edited manifests; the Rust importer will
                // report detailed errors if the user imports the IPA again.
            }
        }
        return latest;
    }

    private String safeChildName(String name) {
        if (name == null || name.isEmpty() || ".".equals(name) || "..".equals(name)
                || name.indexOf('/') >= 0 || name.indexOf('\\') >= 0) {
            return null;
        }
        return name;
    }

    private String readUtf8(File file) throws IOException {
        StringBuilder result = new StringBuilder();
        try (BufferedReader reader = new BufferedReader(new InputStreamReader(
                new FileInputStream(file), StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) {
                result.append(line).append('\n');
            }
        }
        return result.toString();
    }

    private void startEmulator() {
        final ImportedGame game = importedGame;
        if (game == null || busy || isEmulatorRunning()) {
            return;
        }

        startingEmulator = true;
        stopRequested = false;
        consoleText.setText("");
        setStatus("Starting the Rust emulator…");
        updateButtons();

        worker.execute(() -> {
            String outcome;
            try {
                File binary = requireEmulatorBinary();
                int port = findAvailablePort();
                ProcessBuilder builder = new ProcessBuilder(
                        binary.getAbsolutePath(),
                        "run",
                        game.executable.getAbsolutePath(),
                        "--bundle",
                        game.bundle.getAbsolutePath(),
                        "--dest",
                        gamesRoot.getAbsolutePath(),
                        "--serve",
                        Integer.toString(port),
                        "--stats")
                        .directory(gamesRoot)
                        .redirectErrorStream(true);
                Map<String, String> environment = builder.environment();
                environment.put("HOME", getFilesDir().getAbsolutePath());
                environment.put("TMPDIR", getCacheDir().getAbsolutePath());
                // The app's embedded WebView is the only client. Do not expose
                // the unauthenticated preview/import server to the Wi-Fi LAN.
                environment.put("SIMPSONS_EMU_SERVE_HOST", "127.0.0.1");

                Process process = builder.start();
                emulatorProcess = process;
                runOnUiThread(this::updateButtons);
                if (stopRequested) {
                    process.destroy();
                }

                boolean previewReady = waitForPreview(process, port);
                if (previewReady) {
                    String previewUrl = "http://127.0.0.1:" + port + "/";
                    runOnUiThread(() -> {
                        preview.setVisibility(View.VISIBLE);
                        preview.loadUrl(previewUrl);
                        statusText.setText("Emulator running. The live framebuffer and guest log are shown below.");
                        pageScrollToPreview();
                    });
                } else if (!stopRequested) {
                    appendConsole("Preview server did not become ready; check the startup output below.");
                }

                int exitCode = streamOutputAndWait(process);
                outcome = exitCode == 0
                        ? "Emulator exited. See output for its stop reason."
                        : "Emulator stopped with exit code " + exitCode + ". See output for details.";
            } catch (Exception error) {
                outcome = "Could not start the emulator: "
                        + (error.getMessage() == null ? error.toString() : error.getMessage());
                appendConsole(outcome);
            } finally {
                emulatorProcess = null;
                startingEmulator = false;
                stopRequested = false;
            }

            final String finalOutcome = outcome;
            runOnUiThread(() -> {
                preview.stopLoading();
                preview.setVisibility(View.GONE);
                setStatus(finalOutcome);
                updateButtons();
            });
        });
    }

    private void pageScrollToPreview() {
        if (pageScroller != null && preview != null) {
            pageScroller.post(() -> pageScroller.smoothScrollTo(0, preview.getTop()));
            preview.requestFocus();
        }
    }

    private void stopEmulator() {
        if (!isEmulatorRunning()) {
            return;
        }
        stopRequested = true;
        Process process = emulatorProcess;
        if (process != null) {
            process.destroy();
        }
        setStatus("Stopping emulator…");
        appendConsole("Stop requested.");
        updateButtons();
    }

    private boolean isEmulatorRunning() {
        return startingEmulator || emulatorProcess != null;
    }

    private File requireEmulatorBinary() throws IOException {
        String libraryDir = getApplicationInfo().nativeLibraryDir;
        File binary = new File(libraryDir, "libsimpsons-emu.so");
        if (!binary.isFile()) {
            throw new IOException("The APK has no Rust emulator for this device. Install the universal APK built by android.yml.");
        }
        if (!binary.canExecute()) {
            throw new IOException("Android did not mark the packaged emulator executable. Reinstall the APK.");
        }
        return binary;
    }

    private int findAvailablePort() throws IOException {
        try (ServerSocket socket = new ServerSocket(0)) {
            return socket.getLocalPort();
        }
    }

    private boolean waitForPreview(Process process, int port) {
        long deadline = System.currentTimeMillis() + 12_000L;
        String address = "http://127.0.0.1:" + port + "/stats";
        while (System.currentTimeMillis() < deadline && processIsRunning(process)) {
            HttpURLConnection connection = null;
            try {
                connection = (HttpURLConnection) new URL(address).openConnection();
                connection.setConnectTimeout(500);
                connection.setReadTimeout(500);
                connection.setUseCaches(false);
                if (connection.getResponseCode() == HttpURLConnection.HTTP_OK) {
                    InputStream response = connection.getInputStream();
                    response.close();
                    return true;
                }
            } catch (Exception ignored) {
                // The listener may not be up yet; retry while the child runs.
            } finally {
                if (connection != null) {
                    connection.disconnect();
                }
            }
            try {
                Thread.sleep(250L);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return false;
            }
        }
        return false;
    }

    private boolean processIsRunning(Process process) {
        try {
            process.exitValue();
            return false;
        } catch (IllegalThreadStateException stillRunning) {
            return true;
        }
    }

    private int streamOutputAndWait(Process process) throws IOException, InterruptedException {
        try (BufferedReader reader = new BufferedReader(new InputStreamReader(
                process.getInputStream(), StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) {
                appendConsole(line);
            }
        }
        return process.waitFor();
    }

    private void appendConsole(String line) {
        runOnUiThread(() -> {
            if (consoleText == null) {
                return;
            }
            String current = consoleText.getText().toString();
            String addition = line + "\n";
            int excess = current.length() + addition.length() - MAX_CONSOLE_CHARS;
            if (excess > 0) {
                current = current.substring(Math.min(excess, current.length()));
            }
            consoleText.setText(current + addition);
            if (consoleScroll != null) {
                consoleScroll.post(() -> consoleScroll.fullScroll(View.FOCUS_DOWN));
            }
        });
    }

    private void setStatus(String message) {
        if (statusText != null) {
            statusText.setText(message);
        }
    }

    private void updateButtons() {
        if (chooseButton == null) {
            return;
        }
        boolean running = isEmulatorRunning();
        chooseButton.setEnabled(!busy && !running);
        importButton.setEnabled(!busy && !running && selectedIpaUri != null);
        refreshButton.setEnabled(!busy && !running);
        runButton.setEnabled(!busy && !running && importedGame != null);
        stopButton.setEnabled(running);
    }

    @Override
    protected void onDestroy() {
        Process process = emulatorProcess;
        if (process != null) {
            process.destroy();
        }
        if (webFileCallback != null) {
            webFileCallback.onReceiveValue(null);
            webFileCallback = null;
        }
        worker.shutdownNow();
        if (preview != null) {
            preview.stopLoading();
            preview.destroy();
        }
        super.onDestroy();
    }

    private static final class ImportedGame {
        final File bundle;
        final File executable;
        final String label;
        final long importedAt;

        ImportedGame(File bundle, File executable, String label, long importedAt) {
            this.bundle = bundle;
            this.executable = executable;
            this.label = label;
            this.importedAt = importedAt;
        }
    }
}
