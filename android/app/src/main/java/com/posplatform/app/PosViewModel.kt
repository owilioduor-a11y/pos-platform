package com.posplatform.app

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.posplatform.app.network.ApiClient
import com.posplatform.app.softpos.MockTapToPayManager
import com.posplatform.app.softpos.TapToPayManager
import java.net.URLEncoder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import uniffi.pos_core.PosCore

/** Merchant operating mode selected at onboarding. */
enum class MerchantMode { MICRO, RETAIL }

enum class TapState { IDLE, WAITING, APPROVED, DECLINED }

/**
 * State bridge between the Compose UI and the Rust `pos-core` engine.
 * All monetary values are integer minor units (cents).
 */
class PosViewModel : ViewModel() {

    private val engine: PosCore by lazy {
        // Production: load libpos_core.so and pass real server URL + JWT.
        // For development we point at the local emulator host.
        System.loadLibrary("pos_core")
        PosCore(
            dbPath = "pos_platform.db",
            merchantId = MERCHANT_ID,
            serverUrl = SERVER_URL,
            apiToken = API_TOKEN,
        )
    }

    private val apiClient: ApiClient by lazy { ApiClient(SERVER_URL, API_TOKEN) }

    private val tapManager: TapToPayManager = MockTapToPayManager()

    private val _merchantMode = MutableStateFlow(MerchantMode.MICRO)
    val merchantMode: StateFlow<MerchantMode> = _merchantMode.asStateFlow()

    private val _cartTotalMinor = MutableStateFlow(0L)
    val cartTotalMinor: StateFlow<Long> = _cartTotalMinor.asStateFlow()

    private val _cartSummary = MutableStateFlow("")
    val cartSummary: StateFlow<String> = _cartSummary.asStateFlow()

    private val _tapState = MutableStateFlow(TapState.IDLE)
    val tapState: StateFlow<TapState> = _tapState.asStateFlow()

    private val _lastMessage = MutableStateFlow<String?>(null)
    val lastMessage: StateFlow<String?> = _lastMessage.asStateFlow()

    init {
        refreshCart()
    }

    fun setMode(mode: MerchantMode) {
        _merchantMode.value = mode
    }

    /** Quick Charge: set an ad-hoc amount (minor units) for Micro mode. */
    fun setAdhocAmount(amountMinor: Long) {
        runCatching {
            engine.setAdhocAmount(amountMinor, CURRENCY)
        }.onSuccess { refreshCart() }
            .onFailure { _lastMessage.value = it.message }
    }

    fun scanBarcode(barcode: String) {
        viewModelScope.launch {
            runCatching { engine.scanBarcode(barcode) }
                .onSuccess { found ->
                    if (!found) _lastMessage.value = "Unknown barcode: $barcode"
                    refreshCart()
                }
                .onFailure { _lastMessage.value = it.message }
        }
    }

    fun clearCart() {
        engine.clearCart()
        refreshCart()
    }

    /**
     * Charge the full cart total via NFC tap. The certified SoftPOS SDK
     * performs the EMV transaction; we only handle the masked result.
     */
    fun chargeByTap() {
        val total = _cartTotalMinor.value
        if (total <= 0) {
            _lastMessage.value = "Cart is empty"
            return
        }
        viewModelScope.launch {
            _tapState.value = TapState.WAITING
            val result = tapManager.charge(amountMinor = total, currency = CURRENCY)
            _tapState.value = if (result.approved) TapState.APPROVED else TapState.DECLINED

            if (result.approved) {
                // Record masked EMV metadata — never PAN/CVV.
                engine.recordTap(
                    amountMinor = total,
                    currencyCode = CURRENCY,
                    scheme = result.scheme,
                    funding = result.funding,
                    panLast4 = result.panLast4,
                    authCode = result.authCode,
                    processorRef = result.processorRef,
                )
                checkout(listOf(
                    mapOf(
                        "method" to "card_tap",
                        "amount_minor" to total,
                        "reference" to result.processorRef,
                    )
                ))
            } else {
                _lastMessage.value = result.declineReason ?: "Card declined"
            }
            delay(1500)
            _tapState.value = TapState.IDLE
        }
    }

    /** Trigger an M-Pesa STK Push and complete checkout after the callback. */
    fun chargeByMpesa(phone: String) {
        val total = _cartTotalMinor.value
        if (total <= 0) {
            _lastMessage.value = "Cart is empty"
            return
        }
        val customerPhone = phone.trim()
        if (customerPhone.isEmpty()) {
            _lastMessage.value = "Phone number required"
            return
        }
        viewModelScope.launch {
            try {
                val request = JSONObject()
                    .put("phone", customerPhone)
                    .put("amount_minor", total)
                    .put("currency_code", CURRENCY)
                    .put("idempotency_key", engine.newIdempotencyKey())
                val push = withContext(Dispatchers.IO) {
                    apiClient.post("/api/v1/payments/mpesa/stkpush", request)
                }
                val checkoutId = push.optString("checkout_request_id").orEmpty()
                if (!push.optBoolean("approved") || checkoutId.isEmpty()) {
                    val reason = push.optString("response_description").orEmpty()
                    _lastMessage.value = reason.ifEmpty { "STK Push was not accepted" }
                    return@launch
                }

                _lastMessage.value = "STK Push sent; waiting for confirmation"
                val encodedId = URLEncoder.encode(checkoutId, "UTF-8")
                var settled = false
                var attempts = 0
                while (attempts < 15 && !settled) {
                    val status = withContext(Dispatchers.IO) {
                        apiClient.get("/api/v1/payments/mpesa/status/$encodedId")
                    }
                    when (status.optString("status")) {
                        "completed" -> {
                            settled = true
                            checkout(listOf(
                                mapOf(
                                    "method" to "mpesa",
                                    "amount_minor" to total,
                                    "reference" to checkoutId,
                                )
                            ))
                        }
                        "failed" -> {
                            settled = true
                            _lastMessage.value = "M-Pesa payment failed"
                        }
                        else -> delay(2000)
                    }
                    attempts++
                }
                if (!settled) {
                    _lastMessage.value = "M-Pesa payment still pending; check again later"
                }
            } catch (e: Exception) {
                _lastMessage.value = e.message ?: "M-Pesa payment failed"
            }
        }
    }

    /** Cash payment — completes the sale immediately. */
    fun chargeCash() {
        val total = _cartTotalMinor.value
        if (total <= 0) {
            _lastMessage.value = "Cart is empty"
            return
        }
        checkout(listOf(
            mapOf(
                "method" to "cash",
                "amount_minor" to total,
                "reference" to "cash_${System.currentTimeMillis()}",
            )
        ))
    }

    private fun checkout(legs: List<Map<String, Any>>) {
        viewModelScope.launch {
            runCatching {
                val json = org.json.JSONArray(legs.map { org.json.JSONObject(it) })
                    .toString()
                engine.checkout(json, engine.newIdempotencyKey())
            }.onSuccess { saleId ->
                _lastMessage.value = "Sale complete: ${saleId.take(8)}"
                refreshCart()
                syncNow()
            }.onFailure { _lastMessage.value = it.message }
        }
    }

    fun syncNow() {
        viewModelScope.launch {
            runCatching { engine.syncPending() }
                .onSuccess { n -> if (n > 0u) _lastMessage.value = "Synced $n records" }
                .onFailure { _lastMessage.value = "Sync failed: ${it.message}" }
        }
    }

    private fun refreshCart() {
        _cartSummary.value = engine.cartSummary()
        _cartTotalMinor.value = runCatching { engine.cartTotalMinor() }.getOrDefault(0L)
    }

    companion object {
        private const val CURRENCY = "KES"
        private const val SERVER_URL = "http://10.0.2.2:8080"
        private const val API_TOKEN = "dev-token"
        private const val MERCHANT_ID = "00000000-0000-7000-8000-000000000001"
    }
}
