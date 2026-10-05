package com.simpsonsswift.emulator;

import android.util.JsonReader;
import android.util.JsonToken;

import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.List;

/**
 * Reads the game library the emulator's importer writes.
 *
 * <pre>
 * files/games/&lt;bundle id&gt;/import.json          written by `simpsons-emu import`
 * files/games/&lt;bundle id&gt;/&lt;app bundle&gt;/&lt;exe&gt;   the extracted iOS bundle
 * </pre>
 *
 * The manifest is the emulator's own format, so the app parses it rather than
 * guessing at directory names.
 */
final class GameLibrary {

    static final class Game {
        final String label;
        final File bundleDir;
        final File executable;

        Game(String label, File bundleDir, File executable) {
            this.label = label;
            this.bundleDir = bundleDir;
            this.executable = executable;
        }
    }

    private GameLibrary() {
    }

    /** Every imported game whose executable is actually on disk. */
    static List<Game> scan(File root) {
        List<Game> games = new ArrayList<>();
        File[] entries = root.listFiles();
        if (entries == null) {
            return games;
        }
        for (File directory : entries) {
            if (!directory.isDirectory()) {
                continue;
            }
            File manifest = new File(directory, "import.json");
            if (!manifest.isFile()) {
                continue;
            }
            Game game = read(directory, manifest);
            if (game != null) {
                games.add(game);
            }
        }
        Collections.sort(games, new Comparator<Game>() {
            @Override
            public int compare(Game left, Game right) {
                return left.label.compareToIgnoreCase(right.label);
            }
        });
        return games;
    }

    private static Game read(File directory, File manifest) {
        String appBundle = null;
        String executable = null;
        String title = null;
        String displayName = null;
        String version = null;

        InputStream stream = null;
        JsonReader reader = null;
        try {
            stream = new FileInputStream(manifest);
            reader = new JsonReader(new InputStreamReader(stream, "UTF-8"));
            reader.beginObject();
            while (reader.hasNext()) {
                String name = reader.nextName();
                if (reader.peek() == JsonToken.NULL) {
                    reader.nextNull();
                    continue;
                }
                if ("app_bundle".equals(name)) {
                    appBundle = reader.nextString();
                } else if ("executable".equals(name)) {
                    executable = reader.nextString();
                } else if ("title".equals(name)) {
                    title = reader.nextString();
                } else if ("display_name".equals(name)) {
                    displayName = reader.nextString();
                } else if ("version".equals(name)) {
                    version = reader.nextString();
                } else {
                    reader.skipValue();
                }
            }
            reader.endObject();
        } catch (IOException | IllegalStateException | NumberFormatException unreadable) {
            return null;
        } finally {
            close(reader);
            close(stream);
        }

        if (appBundle == null || executable == null) {
            return null;
        }
        File bundleDir = new File(directory, appBundle);
        File binary = new File(bundleDir, executable);
        if (!binary.isFile()) {
            return null;
        }

        String name = displayName != null ? displayName : (title != null ? title : directory.getName());
        String label = version != null ? name + " " + version : name;
        return new Game(label, bundleDir, binary);
    }

    private static void close(Object closeable) {
        if (closeable instanceof JsonReader) {
            try {
                ((JsonReader) closeable).close();
            } catch (IOException ignored) {
                // Reading is over either way.
            }
        } else if (closeable instanceof InputStream) {
            try {
                ((InputStream) closeable).close();
            } catch (IOException ignored) {
                // Reading is over either way.
            }
        }
    }
}
