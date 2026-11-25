package com.posplatform.app

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import com.posplatform.app.ui.QuickChargeScreen
import com.posplatform.app.ui.RetailCatalogScreen
import com.posplatform.app.ui.TapPromptSheet

/** Top-level navigation between merchant modes. */
@Composable
fun PosApp(viewModel: PosViewModel) {
    val mode by viewModel.merchantMode.collectAsState()

    Scaffold(modifier = Modifier.fillMaxSize()) { padding ->
        Box(modifier = Modifier.fillMaxSize()) {
            when (mode) {
                MerchantMode.MICRO -> QuickChargeScreen(
                    viewModel = viewModel,
                    contentPadding = padding,
                )
                MerchantMode.RETAIL -> RetailCatalogScreen(
                    viewModel = viewModel,
                    contentPadding = padding,
                )
            }
            TapPromptSheet(viewModel = viewModel)
        }
    }
}
