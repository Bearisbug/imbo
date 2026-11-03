package dev.imbo.android.ui.components

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.imbo.android.data.Account
import dev.imbo.android.data.ImboApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * 顶部品牌栏要显示试用/订阅状态，但之前是写死的 20 天。
 * 这里单独拉一次账号，让自建服务端的 free 账号不至于显示成「试用还剩 20 天」。
 */
@HiltViewModel
class BrandBarViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {

    private val _account = MutableStateFlow<Account?>(null)
    val account: StateFlow<Account?> = _account.asStateFlow()

    init {
        viewModelScope.launch {
            _account.value = runCatching { api.me() }.getOrNull()
            api.changes.collect {
                _account.value = runCatching { api.me() }.getOrNull()
            }
        }
    }
}
