package dev.imbo.android.data

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import javax.inject.Inject
import javax.inject.Singleton

/**
 * 登录态被服务端拒绝（401）时广播一次，由 Activity 负责把用户送回登录页。
 * 放在这里是为了让 HTTP 层不认识导航。
 */
@Singleton
class SessionEvents @Inject constructor() {

    private val _signedOut = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    val signedOut: SharedFlow<Unit> = _signedOut.asSharedFlow()

    fun notifySignedOut() {
        _signedOut.tryEmit(Unit)
    }
}
