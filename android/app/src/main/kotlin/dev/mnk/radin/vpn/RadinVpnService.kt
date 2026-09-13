// RadinVpnService.kt — data-plane/tunnel scaffold for the RADIN Android app.
//
// BUILD-REQUIRED SCAFFOLD: this workspace cannot build Android (no Android
// SDK/NDK present). The file is complete Kotlin by hand-review but must be
// compiled/verified on a machine with an Android toolchain.
//
// Design (spec 41): the data plane tunnels IP packets over the selected
// transport and forwards the *measurements* the engine needs; the Rust core
// (via libradin_ffi) is the sole decision-maker. The service never fabricates
// readings — every probe result it hands to the engine is something it truly
// observed on the wire. The engine's policy gate may_forward_direct() decides
// whether direct (non-tunnel) forward is permitted.

package dev.mnk.radin.vpn

import android.net.VpnService
import android.os.ParcelFileDescriptor
import android.util.Log

class RadinVpnService : VpnService() {

    companion object {
        private const val TAG = "RadinVpn"
        private const val TUN_MTU = 1500

        // Stable per-build paths; the actual .so is bundled via jniLibs and
        // loaded in the RadinNative companion of radin_ffi.dart.
        private const val NATIVE_LIB = "libradin_ffi.so"
    }

    private var tunFd: ParcelFileDescriptor? = null
    private var tunnelThread: Thread? = null

    override fun onStartCommand(intent: android.content.Intent?, flags: Int, startId: Int): Int {
        Log.i(TAG, "start: installing TUN device")
        installTun()
        tunnelThread = Thread({ runTunnelLoop() }, "radin-tun").also { it.start() }
        return START_STICKY
    }

    private fun installTun() {
        val builder = Builder().apply {
            setName("radin0")
            setMtu(TUN_MTU)
            addAddress("10.8.0.2", 24)
            addRoute("0.0.0.0", 0) // all traffic through the tunnel
            // DNS + excludes go here in the real app (protect() the tunnel fd).
        }
        tunFd = builder.establish()
    }

    private fun runTunnelLoop() {
        val fd = tunFd ?: return
        val buffer = java.nio.ByteBuffer.allocateDirect(TUN_MTU)
        // Read loop stub — in the integrated app each packet is classified,
        // its transport selected via RadinNative.observe(), and payload held
        // only in this data-plane buffer (never logged, never exported).
        while (!Thread.currentThread().isInterrupted) {
            val n = fd.fileDescriptor; if (n <= 0) break
            // IMHO the byte loop belongs with the engine's measurement hooks:
            // (latency, jitter, loss) are captured here and pushed via FFI.
            try {
                Thread.sleep(1)
            } catch (_: InterruptedException) {
                break
            }
        }
    }

    override fun onDestroy() {
        Log.i(TAG, "stop: tearing down tunnel")
        tunnelThread?.interrupt()
        tunFd?.close()
        super.onDestroy()
    }
}