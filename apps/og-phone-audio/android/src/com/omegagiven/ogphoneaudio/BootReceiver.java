package com.omegagiven.ogphoneaudio;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** Restarts the relay after a reboot or app update, if the user left autostart on. */
public class BootReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context c, Intent i) {
        if (RelayService.prefs(c).getBoolean("autostart", true)
                && !RelayService.prefs(c).getString("code", "").isEmpty()) {
            RelayService.start(c);
        }
    }
}
