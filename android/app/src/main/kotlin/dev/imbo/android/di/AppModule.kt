package dev.imbo.android.di

import android.content.Context
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import dev.imbo.android.BuildConfig
import dev.imbo.android.data.AttachmentStore
import dev.imbo.android.data.DraftStorage
import dev.imbo.android.data.PrefsDraftStore
import dev.imbo.android.data.ServerConfigStore
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.MockConfig
import dev.imbo.android.data.SessionEvents
import dev.imbo.android.data.SessionState
import dev.imbo.android.data.SessionStore
import dev.imbo.android.data.mock.MockImboApi
import dev.imbo.android.data.remote.AndroidPictureSource
import dev.imbo.android.data.remote.ContentResolverFileSource
import dev.imbo.android.data.remote.ImboHttp
import dev.imbo.android.data.remote.LocalFileSource
import dev.imbo.android.data.remote.PictureSource
import dev.imbo.android.data.remote.RemoteImboApi
import kotlinx.coroutines.flow.MutableStateFlow
import javax.inject.Singleton

/**
 * 数据通道的唯一切换点：界面和 ViewModel 只认 ImboApi。
 * 默认走 mock（验收用）；-Pimbo.useMock=false 切到真实服务端。
 */
@Module
@InstallIn(SingletonComponent::class)
object AppModule {

    @Provides
    @Singleton
    fun mockConfig(): MutableStateFlow<MockConfig> = MutableStateFlow(MockConfig())

    @Provides
    @Singleton
    fun mockApi(config: MutableStateFlow<MockConfig>): MockImboApi = MockImboApi(config)

    @Provides
    @Singleton
    fun sessionState(store: SessionStore): SessionState = store

    @Provides
    @Singleton
    fun imboHttp(
        store: SessionState,
        sessionEvents: SessionEvents,
        @ApplicationContext context: Context,
    ): ImboHttp = ImboHttp(
        defaultBaseUrl = BuildConfig.BASE_URL,
        store = store,
        // 服务器地址可以在 App 里改（登录页与账号页），release 同样生效；
        // 没设过就用构建时固定的 BASE_URL，所以默认行为不变
        overrideBase = { ServerConfigStore.read(context) },
        onUnauthorized = { sessionEvents.notifySignedOut() },
    )

    /** 真实通道上传附件时读本机 content:// URI；mock 通道用不到它。 */
    @Provides
    @Singleton
    fun localFileSource(@ApplicationContext context: Context): LocalFileSource =
        ContentResolverFileSource(context)

    @Provides
    @Singleton
    fun draftStorage(store: PrefsDraftStore): DraftStorage = store

    /** 附件的本地落盘目录，上传与下载共用，按文件 id 命名。 */
    @Provides
    @Singleton
    fun attachmentStore(@ApplicationContext context: Context): AttachmentStore =
        AttachmentStore(java.io.File(context.filesDir, "attachments"))

    /** 真实通道上传头像时读本机图片；与附件分开，便于各自校验格式和大小。 */
    @Provides
    @Singleton
    fun pictureSource(@ApplicationContext context: Context): PictureSource =
        AndroidPictureSource(context)

    @Provides
    @Singleton
    fun imboApi(mock: MockImboApi, remote: RemoteImboApi): ImboApi =
        if (BuildConfig.USE_MOCK) mock else remote
}
