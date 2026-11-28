package com.posplatform.app.ui

import androidx.camera.view.PreviewView
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.posplatform.app.PosViewModel
import com.posplatform.app.hardware.BarcodeScanner

private data class DemoProduct(val sku: String, val name: String, val priceMinor: Long)

/** Demo catalog — replaced by server-synced products in Phase 2. */
private val DEMO_PRODUCTS = listOf(
    DemoProduct("SKU-001", "Soda 500ml", 10000),
    DemoProduct("SKU-002", "Bread loaf", 6500),
    DemoProduct("SKU-003", "Rice 2kg", 25000),
    DemoProduct("SKU-004", "Sugar 1kg", 18000),
    DemoProduct("SKU-005", "Cooking oil 1L", 32000),
    DemoProduct("SKU-006", "Milk 500ml", 8500),
)

/**
 * Retail store mode: product grid, barcode scanner preview, cart total,
 * and tender actions.
 */
@Composable
fun RetailCatalogScreen(
    viewModel: PosViewModel,
    contentPadding: PaddingValues,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val totalMinor by viewModel.cartTotalMinor.collectAsState()
    val message by viewModel.lastMessage.collectAsState()
    var showScanner by remember { mutableStateOf(false) }
    var scanner by remember { mutableStateOf<BarcodeScanner?>(null) }

    DisposableEffect(showScanner) {
        onDispose {
            scanner?.shutdown()
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(contentPadding)
    ) {
        // Header: total + actions
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column {
                Text("Total", style = MaterialTheme.typography.labelMedium)
                Text(
                    "KSh ${formatMinorUnits(totalMinor)}",
                    style = MaterialTheme.typography.headlineMedium,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = { showScanner = !showScanner }) {
                    Text(if (showScanner) "Hide cam" else "Scan")
                }
                Button(onClick = { viewModel.chargeCash() }, enabled = totalMinor > 0) {
                    Text("Cash")
                }
            }
        }

        if (showScanner) {
            AndroidView(
                factory = { ctx ->
                    PreviewView(ctx).also { preview ->
                        scanner = BarcodeScanner(ctx) { code ->
                            viewModel.scanBarcode(code)
                            showScanner = false
                        }.also { it.bindToLifecycle(lifecycleOwner, preview) }
                    }
                },
                modifier = Modifier
                    .fillMaxWidth()
                    .height(220.dp),
            )
        }

        LazyVerticalGrid(
            columns = GridCells.Fixed(3),
            contentPadding = PaddingValues(16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.weight(1f),
        ) {
            items(DEMO_PRODUCTS) { product ->
                Card(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable {
                            // Demo flow: register-free adhoc is micro-mode;
                            // retail adds via product id once catalog sync lands.
                            viewModel.setAdhocAmount(product.priceMinor)
                        }
                ) {
                    Column(modifier = Modifier.padding(12.dp)) {
                        Text(product.name, style = MaterialTheme.typography.titleSmall)
                        Spacer(Modifier.height(4.dp))
                        Text(
                            "KSh ${formatMinorUnits(product.priceMinor)}",
                            color = MaterialTheme.colorScheme.primary,
                        )
                    }
                }
            }
        }

        message?.let {
            Text(
                it,
                modifier = Modifier.padding(16.dp),
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}
