package dev.palmtop.spike;

import android.content.Context;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.util.Log;

import java.nio.charset.StandardCharsets;

/**
 * Wraps Android's NsdManager to discover palmtopd hosts advertised via mDNS
 * (see palmtopd/src/pairing.rs -- the "_palmtop._tcp" service). Resolves each
 * found service to a usable host:port before handing it to the listener,
 * including the host's Noise public key from the service's TXT record --
 * see pairing.rs's doc comment for what trusting an mDNS-sourced key does
 * and doesn't protect against (real, but weaker than a scanned QR).
 *
 * Note: on Android 13+ (API 33+) with a high targetSdkVersion, NSD discovery
 * may require the NEARBY_WIFI_DEVICES permission. Not added to the manifest
 * yet -- untested, since the only device on hand is Android 12 (API 31),
 * which predates that requirement. Flagging rather than guessing.
 */
public class HostDiscovery {
    public interface Listener {
        void onHostFound(String name, String host, int port, String pubkeyHex);
        void onHostLost(String name);
        void onDiscoveryFailed(int errorCode);
    }

    private static final String TAG = "PalmtopDiscovery";
    private static final String SERVICE_TYPE = "_palmtop._tcp.";

    private final NsdManager nsdManager;
    private final Listener listener;
    private NsdManager.DiscoveryListener discoveryListener;

    public HostDiscovery(Context context, Listener listener) {
        this.nsdManager = (NsdManager) context.getApplicationContext().getSystemService(Context.NSD_SERVICE);
        this.listener = listener;
    }

    public void start() {
        stop();
        discoveryListener = new NsdManager.DiscoveryListener() {
            @Override public void onDiscoveryStarted(String serviceType) {
                Log.i(TAG, "discovery started for " + serviceType);
            }

            @Override public void onServiceFound(NsdServiceInfo service) {
                Log.i(TAG, "found: " + service.getServiceName());
                nsdManager.resolveService(service, new NsdManager.ResolveListener() {
                    @Override public void onResolveFailed(NsdServiceInfo info, int errorCode) {
                        Log.w(TAG, "resolve failed for " + info.getServiceName() + ": " + errorCode);
                    }
                    @Override public void onServiceResolved(NsdServiceInfo info) {
                        String host = info.getHost().getHostAddress();
                        int port = info.getPort();
                        String pubkeyHex = "";
                        byte[] pubkeyAttr = info.getAttributes().get("pubkey");
                        if (pubkeyAttr != null) {
                            pubkeyHex = new String(pubkeyAttr, StandardCharsets.UTF_8);
                        }
                        Log.i(TAG, "resolved " + info.getServiceName() + " -> " + host + ":" + port
                                + " pubkey=" + (pubkeyHex.isEmpty() ? "(none)" : "present"));
                        listener.onHostFound(info.getServiceName(), host, port, pubkeyHex);
                    }
                });
            }

            @Override public void onServiceLost(NsdServiceInfo service) {
                listener.onHostLost(service.getServiceName());
            }

            @Override public void onDiscoveryStopped(String serviceType) {
                Log.i(TAG, "discovery stopped");
            }

            @Override public void onStartDiscoveryFailed(String serviceType, int errorCode) {
                Log.e(TAG, "start discovery failed: " + errorCode);
                listener.onDiscoveryFailed(errorCode);
                nsdManager.stopServiceDiscovery(this);
            }

            @Override public void onStopDiscoveryFailed(String serviceType, int errorCode) {
                Log.w(TAG, "stop discovery failed: " + errorCode);
            }
        };
        nsdManager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, discoveryListener);
    }

    public void stop() {
        if (discoveryListener != null) {
            try {
                nsdManager.stopServiceDiscovery(discoveryListener);
            } catch (Exception ignored) {
                // Already stopped/never fully started -- harmless either way.
            }
            discoveryListener = null;
        }
    }
}
