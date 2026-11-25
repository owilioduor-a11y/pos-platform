package com.posplatform.app.hardware

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothSocket
import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.OutputStream
import java.util.UUID

/**
 * ESC/POS Bluetooth thermal receipt printer helper.
 * Most portable 58mm printers use the standard SPP UUID below.
 */
class ThermalPrinter(private val context: Context) {

    companion object {
        private val SPP_UUID: UUID = UUID.fromString("00001101-0000-1000-8000-00805F9B34FB")

        // ESC/POS command bytes
        private val INIT = byteArrayOf(0x1B, 0x40)
        private val ALIGN_CENTER = byteArrayOf(0x1B, 0x61, 0x01)
        private val ALIGN_LEFT = byteArrayOf(0x1B, 0x61, 0x00)
        private val BOLD_ON = byteArrayOf(0x1B, 0x45, 0x01)
        private val BOLD_OFF = byteArrayOf(0x1B, 0x45, 0x00)
        private val CUT = byteArrayOf(0x1D, 0x56, 0x41, 0x03)
    }

    private var socket: BluetoothSocket? = null
    private var output: OutputStream? = null

    @SuppressLint("MissingPermission")
    suspend fun connect(device: BluetoothDevice): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            socket?.close()
            val s = device.createRfcommSocketToServiceRecord(SPP_UUID)
            s.connect()
            socket = s
            output = s.outputStream
        }
    }

    suspend fun disconnect() = withContext(Dispatchers.IO) {
        runCatching {
            output?.close()
            socket?.close()
        }
        output = null
        socket = null
    }

    /** Print a simple sale receipt. All amounts are major-unit strings. */
    suspend fun printReceipt(
        merchantName: String,
        items: List<Pair<String, String>>, // name -> formatted price
        total: String,
        tendered: String,
        change: String,
        footer: String = "Thank you! Karibu tena.",
    ): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            val out = output ?: error("printer not connected")
            out.write(INIT)
            out.write(ALIGN_CENTER)
            out.write(BOLD_ON)
            out.write("$merchantName\n".toByteArray())
            out.write(BOLD_OFF)
            out.write("--------------------------------\n".toByteArray())
            out.write(ALIGN_LEFT)
            for ((name, price) in items) {
                out.write(String.format("%-24s %10s\n", name.take(24), price).toByteArray())
            }
            out.write("--------------------------------\n".toByteArray())
            out.write(BOLD_ON)
            out.write(String.format("%-16s %10s\n", "TOTAL", total).toByteArray())
            out.write(BOLD_OFF)
            out.write(String.format("%-16s %10s\n", "Tendered", tendered).toByteArray())
            out.write(String.format("%-16s %10s\n", "Change", change).toByteArray())
            out.write("\n".toByteArray())
            out.write(ALIGN_CENTER)
            out.write("$footer\n\n\n".toByteArray())
            out.write(CUT)
            out.flush()
        }
    }

    fun listPairedPrinters(): List<BluetoothDevice> {
        val adapter: BluetoothAdapter? = context
            .getSystemService(Context.BLUETOOTH_SERVICE)
            .let { (it as? BluetoothManager)?.adapter }
        return adapter?.bondedDevices?.toList() ?: emptyList()
    }
}
