package com.omegagiven.ogphoneaudio;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.ServiceInfo;
import android.media.AudioAttributes;
import android.media.AudioDeviceCallback;
import android.media.AudioDeviceInfo;
import android.media.AudioManager;
import android.media.AudioFormat;
import android.media.AudioTimestamp;
import android.media.AudioTrack;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.os.Process;
import android.provider.Settings;

import org.json.JSONObject;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.InterfaceAddress;
import java.net.NetworkInterface;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.TimeUnit;

/**
 * Foreground service that relays PC audio (from og-phone-audio) into the
 * phone's own mixer, so Bluetooth headphones hear PC + phone together.
 *
 * Event-driven so it costs ~nothing when there's no PC to talk to:
 *
 *   no Wi-Fi      → no socket, no threads awake. Waits for a Wi-Fi
 *                   NetworkCallback.
 *   Wi-Fi joined  → SEARCH: broadcast HELLO once a second for 20 s.
 *   PC answered   → LINKED: one unicast HELLO every 4 s (keeps the PC's
 *                   session alive). Nothing else runs; the PC only sends
 *                   audio while something is actually playing.
 *   audio flowing → STREAMING: HELLO (with stats) every second, Wi-Fi
 *                   low-latency lock + AudioTrack held.
 *   PC not found  → IDLE: socket stays bound on :47801 doing nothing.
 *                   The PC pings that port every 10 s while it has no
 *                   phone, which kicks a new SEARCH — so a PC that boots
 *                   after you got home still finds the phone.
 *
 * Direction: every HELLO says whether the phone has its own headphones
 * (Bluetooth other than the PC, wired, USB). The PC uses that to pick:
 * headphones here → PC audio relayed to the phone; none → the PC connects
 * to the phone over Bluetooth as a headset instead, so the phone's calls,
 * notifications and media play on the PC. Headphone changes come from an
 * AudioDeviceCallback (debounced 1.5 s), not polling.
 *
 * No audio focus is requested on purpose — the phone's own music, calls
 * and notifications keep playing alongside the PC stream.
 */
public class RelayService extends Service {
    static final int PC_PORT = 47800;
    static final int PHONE_PORT = 47801;
    static final int RATE = 48000;
    static final byte[] MAGIC = {'O', 'G', 'P', 'A'};
    static final byte T_HELLO = 1, T_AUDIO = 2, T_ACK = 3, T_BYE = 4, T_PING = 5;
    static final long SEARCH_MS = 20_000;
    static final long LINK_TIMEOUT_MS = 10_000;
    static final long STREAM_IDLE_MS = 2_000;
    static final String CHANNEL = "relay";
    static final int NOTIF_ID = 1;

    // ── read by MainActivity for its status panel ──
    static volatile RelayService instance;
    static volatile boolean running;
    static volatile String phase = "Stopped";
    static volatile String pcName = "";
    /** what the PC decided: "relay" / "bluetooth" / "off" */
    static volatile String route = "";
    static volatile String pcBtName = "";
    static volatile boolean headset;
    static volatile String outputs = "";
    static volatile long lastAckMs, lastAudioMs;
    static volatile float bufMs, outMs, frameMs;
    static volatile long received, lost, late, underruns;

    private volatile boolean stop;
    private volatile InetAddress pcAddr;
    private volatile DatagramSocket sock;   // null whenever there's no Wi-Fi
    private volatile Network wifi;
    private volatile long searchUntil;
    private final Object wake = new Object();
    private final ArrayBlockingQueue<byte[]> queue = new ArrayBlockingQueue<>(512);
    private final List<Thread> threads = new ArrayList<>();
    private ConnectivityManager.NetworkCallback netCb;
    private WifiManager.WifiLock wifiLock;
    private PowerManager.WakeLock wakeLock;
    private String lastNotifText = "";
    private final Handler main = new Handler(Looper.getMainLooper());
    private final Runnable headsetChanged = () -> {
        scanOutputs();
        synchronized (wake) { wake.notifyAll(); } // tell the PC right away
    };
    private final AudioDeviceCallback devCb = new AudioDeviceCallback() {
        @Override
        public void onAudioDevicesAdded(AudioDeviceInfo[] d) { debounce(); }

        @Override
        public void onAudioDevicesRemoved(AudioDeviceInfo[] d) { debounce(); }

        private void debounce() {
            main.removeCallbacks(headsetChanged);
            main.postDelayed(headsetChanged, 1500);
        }
    };

    static SharedPreferences prefs(Context c) {
        return c.getSharedPreferences("relay", MODE_PRIVATE);
    }

    static void start(Context c) {
        Intent i = new Intent(c, RelayService.class);
        if (Build.VERSION.SDK_INT >= 26) c.startForegroundService(i);
        else c.startService(i);
    }

    /** Look for the PC right now (app opened, code changed, PC pinged us). */
    static void kick() {
        RelayService s = instance;
        if (s != null) s.search();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (running) {
            search();
            return START_STICKY;
        }
        running = true;
        stop = false;
        instance = this;

        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Relay", NotificationManager.IMPORTANCE_MIN));
        Notification n = notif("Waiting for Wi-Fi");
        if (Build.VERSION.SDK_INT >= 29) startForeground(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE);
        else startForeground(NOTIF_ID, n);

        WifiManager wm = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        // LOW_LATENCY turns off Wi-Fi power save while held — cuts tens of
        // ms of jitter. Held only while audio is actually flowing.
        wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "og:relay");
        wifiLock.setReferenceCounted(false);
        wakeLock = getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "og:relay");
        wakeLock.setReferenceCounted(false);

        scanOutputs();
        getSystemService(AudioManager.class).registerAudioDeviceCallback(devCb, main);

        spawn("control", this::controlLoop);
        spawn("play", this::playLoop);

        ConnectivityManager cm = getSystemService(ConnectivityManager.class);
        netCb = new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
                openSocket(network);
            }

            @Override
            public void onLost(Network network) {
                if (network.equals(wifi)) closeSocket();
            }
        };
        cm.registerNetworkCallback(
                new NetworkRequest.Builder().addTransportType(NetworkCapabilities.TRANSPORT_WIFI).build(), netCb);
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        stop = true;
        running = false;
        instance = null;
        if (netCb != null) {
            try { getSystemService(ConnectivityManager.class).unregisterNetworkCallback(netCb); } catch (Exception ignored) {}
        }
        try { getSystemService(AudioManager.class).unregisterAudioDeviceCallback(devCb); } catch (Exception ignored) {}
        main.removeCallbacks(headsetChanged);
        closeSocket();
        synchronized (wake) { wake.notifyAll(); }
        for (Thread t : threads) {
            t.interrupt();
            try { t.join(1000); } catch (InterruptedException ignored) {}
        }
        if (wifiLock != null && wifiLock.isHeld()) wifiLock.release();
        if (wakeLock != null && wakeLock.isHeld()) wakeLock.release();
        pcName = "";
        phase = "Stopped";
        super.onDestroy();
    }

    private void spawn(String name, Runnable r) {
        Thread t = new Thread(r, "og-" + name);
        threads.add(t);
        t.start();
    }

    // ── Wi-Fi lifecycle ─────────────────────────────────────────────────

    private synchronized void openSocket(Network network) {
        closeSocket();
        try {
            DatagramSocket s = new DatagramSocket(null);
            s.setReuseAddress(true);
            s.setBroadcast(true);
            s.setReceiveBufferSize(1 << 18);
            try {
                s.bind(new InetSocketAddress(PHONE_PORT));
            } catch (Exception e) {
                s.bind(null); // port taken — still works, the PC just can't ping us
            }
            network.bindSocket(s); // always talk over Wi-Fi, even if mobile data is the default route
            wifi = network;
            sock = s;
            Thread rx = new Thread(() -> rxLoop(s), "og-rx");
            rx.start();
        } catch (Exception e) {
            sock = null;
            wifi = null;
            return;
        }
        search();
    }

    private synchronized void closeSocket() {
        DatagramSocket s = sock;
        sock = null;
        wifi = null;
        lastAckMs = 0;
        if (s != null) s.close(); // unblocks its rx thread, which then exits
        synchronized (wake) { wake.notifyAll(); }
    }

    private void search() {
        searchUntil = System.currentTimeMillis() + SEARCH_MS;
        synchronized (wake) { wake.notifyAll(); }
    }

    /** Does the phone have its own headphones? (The PC itself, connected as a
     *  Bluetooth headset in the reverse direction, doesn't count.) */
    private void scanOutputs() {
        boolean found = false;
        StringBuilder names = new StringBuilder();
        for (AudioDeviceInfo d : getSystemService(AudioManager.class).getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
            int t = d.getType();
            boolean bt = t == AudioDeviceInfo.TYPE_BLUETOOTH_A2DP
                    || (Build.VERSION.SDK_INT >= 31 && (t == AudioDeviceInfo.TYPE_BLE_HEADSET || t == AudioDeviceInfo.TYPE_BLE_SPEAKER));
            boolean wired = t == AudioDeviceInfo.TYPE_WIRED_HEADSET || t == AudioDeviceInfo.TYPE_WIRED_HEADPHONES
                    || t == AudioDeviceInfo.TYPE_USB_HEADSET;
            if (!bt && !wired) continue;
            String name = String.valueOf(d.getProductName());
            if (bt && (name.equals(pcBtName) || name.equals(pcName))) continue;
            if (names.indexOf(name) >= 0) continue; // A2DP + BLE can list the same buds twice
            found = true;
            if (names.length() > 0) names.append(", ");
            names.append(name);
        }
        headset = found;
        outputs = names.toString();
    }

    // ── notification ────────────────────────────────────────────────────

    private Notification notif(String text) {
        PendingIntent pi = PendingIntent.getActivity(this, 0, new Intent(this, MainActivity.class), PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Builder(this, CHANNEL)
                .setContentTitle("OG Phone Audio")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_lock_silent_mode_off)
                .setContentIntent(pi)
                .setOngoing(true)
                .build();
    }

    private void setNotif(String text) {
        if (text.equals(lastNotifText)) return;
        lastNotifText = text;
        getSystemService(NotificationManager.class).notify(NOTIF_ID, notif(text));
    }

    private String deviceName() {
        String n = Settings.Global.getString(getContentResolver(), "device_name");
        return (n == null || n.isEmpty()) ? Build.MODEL : n;
    }

    /** Limited broadcast plus each interface's directed broadcast — some APs drop one or the other. */
    private List<InetAddress> broadcastTargets() {
        List<InetAddress> out = new ArrayList<>();
        try {
            out.add(InetAddress.getByName("255.255.255.255"));
            for (NetworkInterface ni : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!ni.isUp() || ni.isLoopback()) continue;
                for (InterfaceAddress ia : ni.getInterfaceAddresses()) {
                    if (ia.getBroadcast() != null) out.add(ia.getBroadcast());
                }
            }
        } catch (Exception ignored) {}
        return out;
    }

    private byte[] helloPacket(String name) throws Exception {
        JSONObject j = new JSONObject();
        j.put("name", name);
        j.put("code", prefs(this).getString("code", ""));
        j.put("buf_ms", bufMs);
        j.put("out_ms", outMs);
        j.put("underruns", underruns);
        j.put("lost", lost);
        j.put("late", late);
        j.put("headset", headset);
        org.json.JSONArray outs = new org.json.JSONArray();
        if (!outputs.isEmpty()) for (String o : outputs.split(", ")) outs.put(o);
        j.put("outputs", outs);
        byte[] body = j.toString().getBytes(StandardCharsets.UTF_8);
        byte[] pkt = new byte[5 + body.length];
        System.arraycopy(MAGIC, 0, pkt, 0, 4);
        pkt[4] = T_HELLO;
        System.arraycopy(body, 0, pkt, 5, body.length);
        return pkt;
    }

    // ── control: decides what (if anything) to send, then sleeps ────────

    private void controlLoop() {
        String name = deviceName();
        while (!stop) {
            long now = System.currentTimeMillis();
            DatagramSocket s = sock;
            boolean streaming = s != null && now - lastAudioMs < STREAM_IDLE_MS + 1000;
            boolean linked = s != null && pcAddr != null && now - lastAckMs < LINK_TIMEOUT_MS;
            boolean searching = s != null && now < searchUntil;
            boolean hasCode = !prefs(this).getString("code", "").isEmpty();

            long sleep; // 0 = sleep until something happens
            List<InetAddress> targets = null;
            if (s == null) {
                phase = "No Wi-Fi — idle";
                sleep = 0;
            } else if (!hasCode) {
                phase = "Enter the pair code";
                sleep = 0;
            } else if (streaming) {
                phase = "Streaming from " + pcName;
                targets = Collections.singletonList(pcAddr);
                sleep = 1000;
            } else if (linked && route.equals("bluetooth")) {
                phase = "Phone audio → " + pcName + " over Bluetooth";
                targets = Collections.singletonList(pcAddr);
                sleep = 4000;
            } else if (linked && route.equals("off")) {
                phase = "Connected to " + pcName + " — no headphones on the phone";
                targets = Collections.singletonList(pcAddr);
                sleep = 4000;
            } else if (linked) {
                phase = "Connected to " + pcName + " (nothing playing)";
                targets = Collections.singletonList(pcAddr);
                sleep = 4000;
            } else if (searching) {
                phase = "Looking for your PC…";
                targets = broadcastTargets();
                sleep = 1000;
            } else {
                phase = "PC not on this Wi-Fi — idle";
                sleep = 0;
            }

            if (targets != null) {
                try {
                    byte[] pkt = helloPacket(name);
                    for (InetAddress a : targets) {
                        try { s.send(new DatagramPacket(pkt, pkt.length, a, PC_PORT)); } catch (Exception ignored) {}
                    }
                } catch (Exception ignored) {}
            }

            if (streaming) {
                if (!wifiLock.isHeld()) wifiLock.acquire();
                if (!wakeLock.isHeld()) wakeLock.acquire();
            } else {
                if (wifiLock.isHeld()) wifiLock.release();
                if (wakeLock.isHeld()) wakeLock.release();
            }
            setNotif(phase);

            synchronized (wake) {
                try {
                    if (sleep == 0) wake.wait();
                    else wake.wait(sleep);
                } catch (InterruptedException e) {
                    return;
                }
            }
        }
    }

    // ── receive: blocks in recv(), no timers ────────────────────────────

    private void rxLoop(DatagramSocket s) {
        byte[] buf = new byte[8192];
        DatagramPacket p = new DatagramPacket(buf, buf.length);
        long expect = -1;
        while (!stop && s == sock) {
            try {
                p.setLength(buf.length);
                s.receive(p);
            } catch (Exception e) {
                return; // socket closed (Wi-Fi lost / service stopping)
            }
            int n = p.getLength();
            if (n < 5 || buf[0] != 'O' || buf[1] != 'G' || buf[2] != 'P' || buf[3] != 'A') continue;
            long now = System.currentTimeMillis();
            switch (buf[4]) {
                case T_AUDIO: {
                    if (n < 16) break;
                    int frames = (buf[6] & 0xff) | (buf[7] & 0xff) << 8;
                    long seq = ((buf[8] & 0xffL) | (buf[9] & 0xffL) << 8 | (buf[10] & 0xffL) << 16 | (buf[11] & 0xffL) << 24);
                    if (expect >= 0) {
                        long gap = (seq - expect) & 0xffffffffL;
                        if (gap > 0 && gap < 1000) lost += gap;
                        else if (gap >= 0x80000000L) { late++; break; } // older than one we already played
                    }
                    expect = (seq + 1) & 0xffffffffL;
                    frameMs = frames * 1000f / RATE;
                    pcAddr = p.getAddress();
                    boolean wasIdle = now - lastAudioMs > STREAM_IDLE_MS;
                    lastAudioMs = now;
                    lastAckMs = now; // audio proves the link
                    received++;
                    byte[] pcm = new byte[n - 16];
                    System.arraycopy(buf, 16, pcm, 0, pcm.length);
                    if (!queue.offer(pcm)) { queue.poll(); queue.offer(pcm); late++; }
                    if (wasIdle) synchronized (wake) { wake.notifyAll(); }
                    break;
                }
                case T_ACK: {
                    try {
                        JSONObject j = new JSONObject(new String(buf, 5, n - 5, StandardCharsets.UTF_8));
                        pcName = j.optString("host", "PC");
                        route = j.optString("route", "relay");
                        String btn = j.optString("bt_name", "");
                        if (!btn.equals(pcBtName)) {
                            pcBtName = btn;
                            main.post(this::scanOutputs); // now we know which BT device is the PC
                        }
                    } catch (Exception ignored) {}
                    boolean wasUnlinked = now - lastAckMs > LINK_TIMEOUT_MS;
                    pcAddr = p.getAddress();
                    lastAckMs = now;
                    searchUntil = 0;
                    if (wasUnlinked) synchronized (wake) { wake.notifyAll(); }
                    break;
                }
                case T_PING:
                    // PC (re)started and is looking for us
                    if (now - lastAckMs > LINK_TIMEOUT_MS && now >= searchUntil) search();
                    break;
                case T_BYE:
                    lastAckMs = 0;
                    lastAudioMs = 0;
                    expect = -1;
                    synchronized (wake) { wake.notifyAll(); }
                    break;
            }
        }
    }

    // ── playback ────────────────────────────────────────────────────────

    private AudioTrack newTrack() {
        int min = AudioTrack.getMinBufferSize(RATE, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT);
        AudioTrack t = new AudioTrack.Builder()
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build())
                .setAudioFormat(new AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .build())
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(min)
                .build();
        t.play();
        return t;
    }

    private void playLoop() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO);
        AudioTrack track = null;
        AudioTimestamp ts = new AudioTimestamp();
        long written = 0;
        boolean starved = true;
        int baseUnderruns = 0;
        int windowMin = Integer.MAX_VALUE;
        long windowStart = System.currentTimeMillis();
        while (!stop) {
            byte[] pcm;
            try {
                // No track = nothing playing: block until audio arrives
                // instead of waking up on a timer.
                pcm = track == null ? queue.take() : queue.poll(500, TimeUnit.MILLISECONDS);
            } catch (InterruptedException e) {
                break;
            }
            if (pcm == null) {
                starved = true;
                // PC went quiet or away: let go of the output so Bluetooth can idle
                if (System.currentTimeMillis() - lastAudioMs > STREAM_IDLE_MS) {
                    track.stop();
                    track.release();
                    track = null;
                    bufMs = 0;
                    outMs = 0;
                }
                continue;
            }
            if (track == null) {
                track = newTrack();
                written = 0;
                baseUnderruns = track.getUnderrunCount();
                windowMin = Integer.MAX_VALUE;
                windowStart = System.currentTimeMillis();
            }

            float fm = frameMs > 0 ? frameMs : 5f;
            int target = prefs(this).getInt("target_ms", 20);

            // After a gap, let a little backlog build so one late packet
            // doesn't immediately underrun again.
            if (starved) {
                long until = System.currentTimeMillis() + target;
                while (!stop && queue.size() * fm < target / 2f && System.currentTimeMillis() < until) {
                    try { Thread.sleep(1); } catch (InterruptedException e) { break; }
                }
                starved = false;
            }

            // Clock drift makes the backlog creep up over time; Wi-Fi bursts
            // and the Bluetooth mixer's big pulls make it spike briefly.
            // Only trim drift: if the *minimum* backlog over the last second
            // stayed above target, drop just the excess. Hard cap catches a
            // real pile-up (e.g. after the phone stalled).
            int depth = queue.size();
            windowMin = Math.min(windowMin, depth);
            long nowMs = System.currentTimeMillis();
            if (nowMs - windowStart >= 1000) {
                int excess = windowMin - (int) Math.ceil(target / fm);
                for (int i = 0; i < excess; i++) {
                    if (queue.poll() != null) late++;
                }
                windowMin = Integer.MAX_VALUE;
                windowStart = nowMs;
            }
            while (queue.size() * fm > target * 3 + 50) {
                queue.poll();
                late++;
            }
            bufMs = queue.size() * fm;

            track.write(pcm, 0, pcm.length, AudioTrack.WRITE_BLOCKING);
            written += pcm.length / 4;
            underruns = track.getUnderrunCount() - baseUnderruns;

            if (track.getTimestamp(ts)) {
                double pipe = (written - ts.framePosition) * 1000.0 / RATE;
                double age = (System.nanoTime() - ts.nanoTime) / 1e6;
                outMs = (float) Math.max(0, pipe - age);
            }
        }
        if (track != null) {
            track.stop();
            track.release();
        }
    }
}
