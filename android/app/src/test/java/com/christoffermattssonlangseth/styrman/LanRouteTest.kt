package com.christoffermattssonlangseth.styrman

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class LanRouteTest {
    private val fp = "ab".repeat(32)

    @Test
    fun helloCarriesTheLocalNetworkRoute() {
        val msg = ServerMsg.parse(
            """{"type":"hello","protocol":1,"daemon_version":"0.1.0","device_id":"D","host":"mac",
               "lan_urls":["wss://192.168.1.20:4720/v1/ws","wss://8.8.8.8:4720/v1/ws","ws://10.0.0.2:4720/v1/ws"],
               "lan_cert":"$fp"}""",
        ) as ServerMsg.Hello
        assertEquals(listOf("wss://192.168.1.20:4720/v1/ws"), msg.lan?.urls)
        assertEquals(fp, msg.lan?.cert)
        val old = ServerMsg.parse("""{"type":"hello","protocol":1,"daemon_version":"0.1.0","device_id":"D","host":"mac"}""")
        assertNull((old as ServerMsg.Hello).lan)
    }

    @Test
    fun needsAPrivateAddressAndAFingerprint() {
        assertNotNull(LanRoute.of(listOf("wss://10.1.2.3:4720/v1/ws"), fp))
        assertNotNull(LanRoute.of(listOf("wss://172.20.0.9:4720/v1/ws"), fp))
        assertNull(LanRoute.of(listOf("wss://172.32.0.9:4720/v1/ws"), fp))
        assertNull(LanRoute.of(listOf("wss://10.1.2.3:4720/v1/ws"), null))
        assertNull(LanRoute.of(listOf("wss://10.1.2.3:4720/v1/ws"), fp.uppercase()))
        assertEquals(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            PinnedTrust.fingerprint("abc".toByteArray()),
        )
    }
}
