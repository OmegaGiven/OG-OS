package com.omegagiven.ogphoneaudio;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import android.provider.Settings;
import android.text.Editable;
import android.text.InputType;
import android.text.TextWatcher;
import android.util.TypedValue;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.SeekBar;
import android.widget.Switch;
import android.widget.TextView;

/** One screen: pair code, buffer target, start/stop, live stats. Built in code — no XML resources. */
public class MainActivity extends Activity {
    private final Handler ui = new Handler(Looper.getMainLooper());
    private TextView status, targetLabel;
    private Button toggle;
    private SharedPreferences prefs;

    private int dp(int v) {
        return (int) TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, getResources().getDisplayMetrics());
    }

    private TextView label(String s, int sp) {
        TextView t = new TextView(this);
        t.setText(s);
        t.setTextSize(sp);
        t.setPadding(0, dp(14), 0, dp(4));
        return t;
    }

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        prefs = RelayService.prefs(this);

        LinearLayout col = new LinearLayout(this);
        col.setOrientation(LinearLayout.VERTICAL);
        col.setPadding(dp(20), dp(28), dp(20), dp(20));

        TextView title = label("OG Phone Audio", 24);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        col.addView(title);
        col.addView(label("Plays your PC's audio through this phone, mixed with the phone's own sound, so your Bluetooth headphones hear both.", 14));

        status = label("", 15);
        status.setTypeface(Typeface.MONOSPACE);
        col.addView(status);

        col.addView(label("Pair code (og-settings → Audio → Phone)", 14));
        EditText code = new EditText(this);
        code.setInputType(InputType.TYPE_CLASS_NUMBER);
        code.setText(prefs.getString("code", ""));
        code.addTextChangedListener(new TextWatcher() {
            public void beforeTextChanged(CharSequence s, int a, int c, int d) {}
            public void onTextChanged(CharSequence s, int a, int c, int d) {}
            public void afterTextChanged(Editable s) {
                prefs.edit().putString("code", s.toString().trim()).apply();
                RelayService.kick();
            }
        });
        col.addView(code);

        targetLabel = label("", 14);
        col.addView(targetLabel);
        SeekBar target = new SeekBar(this);
        target.setMax(75); // 5..80 ms
        target.setProgress(prefs.getInt("target_ms", 20) - 5);
        target.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            public void onProgressChanged(SeekBar s, int p, boolean user) {
                prefs.edit().putInt("target_ms", p + 5).apply();
                updateTargetLabel();
            }
            public void onStartTrackingTouch(SeekBar s) {}
            public void onStopTrackingTouch(SeekBar s) {}
        });
        col.addView(target);
        updateTargetLabel();

        Switch auto = new Switch(this);
        auto.setText("Start automatically when the phone boots");
        auto.setChecked(prefs.getBoolean("autostart", true));
        auto.setOnCheckedChangeListener((v, on) -> prefs.edit().putBoolean("autostart", on).apply());
        auto.setPadding(0, dp(14), 0, dp(14));
        col.addView(auto);

        toggle = new Button(this);
        toggle.setOnClickListener(v -> {
            if (RelayService.running) stopService(new Intent(this, RelayService.class));
            else RelayService.start(this);
            ui.postDelayed(this::refresh, 150);
        });
        col.addView(toggle);

        Button battery = new Button(this);
        battery.setText("Let it run in the background (battery)");
        battery.setOnClickListener(v -> {
            PowerManager pm = getSystemService(PowerManager.class);
            if (pm.isIgnoringBatteryOptimizations(getPackageName())) {
                startActivity(new Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS));
            } else {
                startActivity(new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:" + getPackageName())));
            }
        });
        col.addView(battery);

        col.addView(label("Uses no battery off Wi-Fi: it only looks for the PC when the phone joins a network, and goes quiet when nothing is playing. Headphones stay paired to the phone. "
                + "Higher buffer = fewer dropouts, more delay.", 13));

        ScrollView sv = new ScrollView(this);
        sv.addView(col);
        setContentView(sv);

        if (Build.VERSION.SDK_INT >= 33) requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, 1);

        // First launch with a code already entered: just start.
        if (!RelayService.running && !prefs.getString("code", "").isEmpty() && prefs.getBoolean("autostart", true)) {
            RelayService.start(this);
        }
    }

    private void updateTargetLabel() {
        targetLabel.setText("Jitter buffer: " + prefs.getInt("target_ms", 20) + " ms");
    }

    private final Runnable tick = new Runnable() {
        public void run() {
            refresh();
            ui.postDelayed(this, 500);
        }
    };

    @Override
    protected void onResume() {
        super.onResume();
        RelayService.kick(); // opening the app = look for the PC now
        ui.post(tick);
    }

    @Override
    protected void onPause() {
        super.onPause();
        ui.removeCallbacks(tick);
    }

    private void refresh() {
        toggle.setText(RelayService.running ? "Stop relay" : "Start relay");
        long now = System.currentTimeMillis();
        String s = "● " + RelayService.phase;
        if (RelayService.running) {
            s += "\n  headphones: " + (RelayService.headset ? RelayService.outputs : "none");
        }
        if (RelayService.running && now - RelayService.lastAudioMs < 3000) {
            s += String.format("\n  packet   %.1f ms", RelayService.frameMs)
                    + String.format("\n  buffer   %.1f ms", RelayService.bufMs)
                    + String.format("\n  output   %.0f ms  (incl. Bluetooth)", RelayService.outMs)
                    + "\n  lost " + RelayService.lost + " · dropped " + RelayService.late + " · underruns " + RelayService.underruns;
        }
        status.setText(s);
    }
}
