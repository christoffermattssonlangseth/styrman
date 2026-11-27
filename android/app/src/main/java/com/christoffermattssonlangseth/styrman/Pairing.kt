package com.christoffermattssonlangseth.styrman

import android.content.Context
import android.net.Uri
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.net.URLDecoder
import java.security.KeyStore
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** `styrman://pair?v=1&url=…&code=…&name=…` from `styr pair`. */
data class PairingLink(val wsUrl: String, val code: String, val name: String) {
    /** `ws://host:4719/v1/ws` → `http://host:4719/`. */
    val httpBase: String
        get() {
            val scheme = if (wsUrl.startsWith("wss://")) "https://" else "http://"
            val rest = wsUrl.substringAfter("://")
            return scheme + rest.substringBefore('/') + "/"
        }

    companion object {
        /** Parse the link (also when pasted with surrounding text). */
        fun parse(raw: String): PairingLink? {
            val s = raw.trim().let { t -> Regex("styrman://pair\\?\\S+").find(t)?.value ?: t }
            if (!s.startsWith("styrman://pair?")) return null
            val q = s.substringAfter('?').split('&').associate {
                val (k, v) = (it.split('=', limit = 2) + "").take(2)
                k to URLDecoder.decode(v, "UTF-8")
            }
            val url = q["url"]?.takeIf { it.startsWith("ws://") || it.startsWith("wss://") } ?: return null
            val code = q["code"]?.takeIf { it.isNotBlank() } ?: return null
            return PairingLink(url, code, q["name"] ?: "Mac")
        }
    }
}

data class Credentials(
    val wsUrl: String,
    val token: String,
    val deviceId: String,
    val host: String,
    /** Local-network fallback for when Tailscale is down (see [LanRoute]), learned over Tailscale. */
    val lanUrls: List<String> = emptyList(),
    val lanCert: String? = null,
) {
    val lan: LanRoute? get() = LanRoute.of(lanUrls, lanCert)
}

object Pairing {
    private val http = OkHttpClient.Builder().callTimeout(15, TimeUnit.SECONDS).build()

    /** Exchange the one-time code for this device's token. */
    suspend fun pair(link: PairingLink): Credentials = withContext(Dispatchers.IO) {
        val body = buildJsonObject {
            put("code", link.code)
            put("device_name", "${Build.MANUFACTURER} ${Build.MODEL}".trim())
        }.toString().toRequestBody("application/json".toMediaType())
        val req = Request.Builder().url(link.httpBase + "v1/pair").post(body).build()
        http.newCall(req).execute().use { r ->
            when {
                r.code == 401 -> error("That code has expired or was already used. Run `styr pair` again.")
                !r.isSuccessful -> error("The Mac answered ${r.code}.")
            }
            val o = json.parseToJsonElement(r.body.string()).jsonObject
            Credentials(
                wsUrl = link.wsUrl,
                token = o.str("token") ?: error("no token"),
                deviceId = o.str("device_id") ?: "",
                host = o.str("host") ?: link.name,
            )
        }
    }
}

/** Device credentials: the token is encrypted with a key that never leaves the Android Keystore. */
class CredentialStore(context: Context) {
    private val prefs = context.getSharedPreferences("styrman", Context.MODE_PRIVATE)

    fun load(): Credentials? {
        val url = prefs.getString("ws_url", null) ?: return null
        val sealed = prefs.getString("token", null) ?: return null
        val token = runCatching { decrypt(sealed) }.getOrNull() ?: return null
        return Credentials(
            url, token, prefs.getString("device_id", "") ?: "", prefs.getString("host", "") ?: "",
            lanUrls = prefs.getString("lan_urls", null)?.split("\n")?.filter { it.isNotEmpty() }.orEmpty(),
            lanCert = prefs.getString("lan_cert", null),
        )
    }

    fun save(c: Credentials) {
        prefs.edit()
            .putString("ws_url", c.wsUrl)
            .putString("token", encrypt(c.token))
            .putString("device_id", c.deviceId)
            .putString("host", c.host)
            .putString("lan_urls", c.lanUrls.joinToString("\n"))
            .putString("lan_cert", c.lanCert)
            .apply()
    }

    fun clear() = prefs.edit().clear().apply()

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build(),
        )
        return gen.generateKey()
    }

    private fun encrypt(plain: String): String {
        val c = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }
        val out = c.iv + c.doFinal(plain.toByteArray())
        return Base64.encodeToString(out, Base64.NO_WRAP)
    }

    private fun decrypt(sealed: String): String {
        val b = Base64.decode(sealed, Base64.NO_WRAP)
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, b, 0, 12))
        return String(c.doFinal(b, 12, b.size - 12))
    }

    private companion object {
        const val ALIAS = "styrman-token"
    }
}

/** Accept a pairing link handed to the app (deep link / share). */
fun Uri.asPairingLink(): PairingLink? = PairingLink.parse(toString())
