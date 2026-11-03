package dev.imbo.android.ui.nav

import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import dev.imbo.android.ui.screen.AccountScreen
import dev.imbo.android.ui.screen.AddContactScreen
import dev.imbo.android.ui.screen.BlockedScreen
import dev.imbo.android.ui.screen.ChatScreen
import dev.imbo.android.ui.screen.ChatsScreen
import dev.imbo.android.ui.screen.CodeScreen
import dev.imbo.android.ui.screen.ContactsScreen
import dev.imbo.android.ui.screen.GroupInfoScreen
import dev.imbo.android.ui.screen.LoginScreen
import dev.imbo.android.ui.screen.NewGroupScreen
import dev.imbo.android.ui.screen.RegisterScreen
import dev.imbo.android.ui.screen.SessionsScreen
import dev.imbo.android.ui.screen.WelcomeScreen

object Routes {
    const val LOGIN = "login"
    const val REGISTER = "register"
    const val CODE = "code?email={email}&name={name}"
    const val WELCOME = "welcome?name={name}"
    const val CHATS = "chats"
    const val SESSIONS = "sessions"
    const val CONTACTS = "contacts"
    const val ACCOUNT = "account"
    const val BLOCKED = "blocked"
    const val ADD_CONTACT = "add-contact"
    const val NEW_GROUP = "new-group"
    const val CHAT = "chat/{conversationId}"
    const val GROUP_INFO = "group/{conversationId}"

    fun code(email: String, name: String? = null) = "code?email=${Uri.encode(email)}&name=${Uri.encode(name.orEmpty())}"
    fun welcome(name: String) = "welcome?name=${Uri.encode(name)}"
    fun chat(id: Long) = "chat/$id"
    fun groupInfo(conversationId: Long) = "group/$conversationId"
}

@Composable
fun ImboNavHost(navController: NavHostController = rememberNavController(), startRoute: String? = null) {
    NavHost(navController = navController, startDestination = startRoute ?: Routes.LOGIN) {
        composable(Routes.LOGIN) {
            LoginScreen(
                onCodeSent = { email -> navController.navigate(Routes.code(email)) },
                onRegister = { navController.navigate(Routes.REGISTER) },
            )
        }
        composable(
            route = Routes.CODE,
            arguments = listOf(
                navArgument("email") { type = NavType.StringType; defaultValue = "" },
                navArgument("name") { type = NavType.StringType; defaultValue = "" },
            ),
        ) { entry ->
            val email = entry.arguments?.getString("email").orEmpty()
            val name = entry.arguments?.getString("name")?.takeIf { it.isNotBlank() }
            CodeScreen(
                email = email,
                name = name,
                onVerified = { accountName, newAccount ->
                    // 老账号直接进对话列表：欢迎页的文案是给刚注册的人看的
                    val target = if (newAccount) Routes.welcome(accountName) else Routes.CHATS
                    navController.navigate(target) {
                        popUpTo(Routes.LOGIN) { inclusive = true }
                    }
                },
                onBack = { navController.popBackStack() },
            )
        }
        composable(Routes.REGISTER) {
            RegisterScreen(
                onCodeSent = { email, name -> navController.navigate(Routes.code(email, name)) },
                onSignIn = { navController.popBackStack() },
            )
        }
        composable(
            route = Routes.WELCOME,
            arguments = listOf(navArgument("name") { type = NavType.StringType; defaultValue = "" }),
        ) { entry ->
            WelcomeScreen(
                name = entry.arguments?.getString("name").orEmpty(),
                onOpen = {
                    navController.navigate(Routes.CHATS) {
                        popUpTo(Routes.WELCOME) { inclusive = true }
                    }
                },
            )
        }
        composable(Routes.CHATS) {
            ChatsScreen(
                onOpenChat = { navController.navigate(Routes.chat(it)) },
                onNewGroup = { navController.navigate(Routes.NEW_GROUP) },
                onAccount = { navController.navigate(Routes.ACCOUNT) },
                onTab = { tab ->
                    when (tab) {
                        "sessions" -> navController.navigate(Routes.SESSIONS)
                        "contacts" -> navController.navigate(Routes.CONTACTS)
                    }
                },
            )
        }
        composable(Routes.SESSIONS) {
            SessionsScreen(
                onOpenChat = { navController.navigate(Routes.chat(it)) },
                onAccount = { navController.navigate(Routes.ACCOUNT) },
                onTab = { tab ->
                    when (tab) {
                        "chats" -> navController.navigate(Routes.CHATS)
                        "contacts" -> navController.navigate(Routes.CONTACTS)
                    }
                },
            )
        }
        composable(Routes.CONTACTS) {
            ContactsScreen(
                onAddContact = { navController.navigate(Routes.ADD_CONTACT) },
                onOpenChat = { navController.navigate(Routes.chat(it)) },
                onAccount = { navController.navigate(Routes.ACCOUNT) },
                onTab = { tab ->
                    when (tab) {
                        "chats" -> navController.navigate(Routes.CHATS)
                        "sessions" -> navController.navigate(Routes.SESSIONS)
                    }
                },
            )
        }
        composable(Routes.CHAT) { entry ->
            val id = entry.arguments?.getString("conversationId")?.toLongOrNull() ?: 1L
            ChatScreen(
                conversationId = id,
                onBack = { navController.popBackStack() },
                onOpenGroupInfo = { navController.navigate(Routes.groupInfo(id)) },
            )
        }
        composable(Routes.GROUP_INFO) { entry ->
            val id = entry.arguments?.getString("conversationId")?.toLongOrNull() ?: 0L
            GroupInfoScreen(
                conversationId = id,
                onBack = { navController.popBackStack() },
                // 退群 / 解散后会话已经没了，一起退回对话列表
                onClosed = {
                    val popped = navController.popBackStack(Routes.CHATS, inclusive = false)
                    if (!popped) navController.navigate(Routes.CHATS)
                },
            )
        }
        composable(Routes.NEW_GROUP) {
            NewGroupScreen(
                onCreated = { conversationId ->
                    navController.popBackStack()
                    if (conversationId != null) navController.navigate(Routes.chat(conversationId))
                },
                onBack = { navController.popBackStack() },
            )
        }
        composable(Routes.ADD_CONTACT) {
            AddContactScreen(onDone = { navController.popBackStack() }, onBack = { navController.popBackStack() })
        }
        composable(Routes.ACCOUNT) {
            AccountScreen(
                onBack = { navController.popBackStack() },
                onBlocked = { navController.navigate(Routes.BLOCKED) },
                onSignedOut = {
                    navController.navigate(Routes.LOGIN) {
                        popUpTo(0) { inclusive = true }
                    }
                },
            )
        }
        composable(Routes.BLOCKED) {
            BlockedScreen(onBack = { navController.popBackStack() })
        }
    }
}
