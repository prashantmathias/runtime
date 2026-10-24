import { Hono } from "hono";
import { createBunWebSocket } from "hono/bun";

console.log("[Hono Test] Initializing Hono app with WebSockets...");

const { upgradeWebSocket, websocket } = createBunWebSocket();
const app = new Hono();

// 1. Middleware test
app.use("*", async (c, next) => {
    await next();
    c.header("x-powered-by", "custom-jsc-runtime");
});

// 2. Route params + query params + JSON response
app.get("/hello/:name", (c) => {
    const name = c.req.param("name");
    const greeting = c.req.query("greeting") || "Hello";
    return c.json({ message: `${greeting}, ${name}!` });
});

// 3. POST JSON body echo
app.post("/echo", async (c) => {
    const body = await c.req.json<{ count: number; text: string }>();
    return c.json({ received: body, doubled: body.count * 2 }, 201);
});

// 4. WebSocket route via hono/bun upgradeWebSocket
const serverEvents: string[] = [];

app.get(
    "/ws",
    upgradeWebSocket((c) => {
        const room = c.req.query("room") || "general";
        return {
            onOpen(_evt, ws) {
                serverEvents.push(`open:${room}`);
                ws.send(`welcome:${room}`);
            },
            onMessage(evt, ws) {
                if (evt.data instanceof ArrayBuffer) {
                    const bytes = new Uint8Array(evt.data);
                    serverEvents.push(`bin:${bytes.length}`);
                    ws.send(new Uint8Array([...bytes].map((b) => b + 1)));
                } else {
                    serverEvents.push(`msg:${evt.data}`);
                    ws.send(`echo:${evt.data}`);
                }
            },
            onClose() {
                serverEvents.push(`close:${room}`);
            },
        };
    })
);

const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch: app.fetch,
    websocket,
});

console.log(`[Hono Test] Server listening on ${server.url.href}`);

async function runTests() {
    const baseUrl = `http://127.0.0.1:${server.port}`;

    // Test 1: GET /hello/:name?greeting=Welcome
    const getRes = await fetch(`${baseUrl}/hello/Bun?greeting=Welcome`);
    if (getRes.status !== 200) {
        throw new Error(`Expected status 200, got ${getRes.status}`);
    }
    if (getRes.headers.get("x-powered-by") !== "custom-jsc-runtime") {
        throw new Error(`Missing middleware header: ${getRes.headers.get("x-powered-by")}`);
    }
    const getJson = await getRes.json();
    if (getJson.message !== "Welcome, Bun!") {
        throw new Error(`Unexpected GET response: ${JSON.stringify(getJson)}`);
    }
    console.log("[Hono Test 1] GET route + params + query + middleware verified:", getJson.message);

    // Test 2: POST /echo
    const postRes = await fetch(`${baseUrl}/echo`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ count: 21, text: "Hono on JSC" }),
    });
    if (postRes.status !== 201) {
        throw new Error(`Expected status 201, got ${postRes.status}`);
    }
    const postJson = await postRes.json();
    if (postJson.doubled !== 42 || postJson.received.text !== "Hono on JSC") {
        throw new Error(`Unexpected POST response: ${JSON.stringify(postJson)}`);
    }
    console.log("[Hono Test 2] POST JSON body + 201 status verified:", JSON.stringify(postJson));

    // Test 3: WebSocket upgrade & bidirectional messaging (text + binary)
    const receivedByClient: string[] = [];
    await new Promise<void>((resolve, reject) => {
        const ws = new WebSocket(`ws://127.0.0.1:${server.port}/ws?room=lobby`);
        ws.binaryType = "arraybuffer";

        ws.onopen = () => {
            console.log("[Hono Test 3.1] Client WebSocket connected");
        };

        ws.onmessage = (evt) => {
            if (evt.data instanceof ArrayBuffer) {
                const arr = Array.from(new Uint8Array(evt.data));
                const tag = `bin:${arr.join("-")}`;
                receivedByClient.push(tag);
                console.log("[Hono Test 3.2] Client received binary:", tag);
                ws.close(1000, "done");
                return;
            }
            const data = String(evt.data);
            receivedByClient.push(data);
            console.log("[Hono Test 3.2] Client received:", data);

            if (data === "welcome:lobby") {
                ws.send("ping-from-client");
            } else if (data === "echo:ping-from-client") {
                ws.send(new Uint8Array([1, 2, 3]));
            }
        };

        ws.onerror = (err) => {
            reject(new Error(`WebSocket error: ${JSON.stringify(err)}`));
        };

        ws.onclose = () => {
            console.log("[Hono Test 3.3] Client WebSocket closed cleanly");
            resolve();
        };
    });

    if (receivedByClient.join(",") !== "welcome:lobby,echo:ping-from-client,bin:2-3-4") {
        throw new Error(`Unexpected WebSocket messages: ${receivedByClient.join(",")}`);
    }

    // Give server close event a tick to settle
    await new Promise((r) => setTimeout(r, 20));
    if (serverEvents.join(",") !== "open:lobby,msg:ping-from-client,bin:3,close:lobby") {
        throw new Error(`Unexpected server WebSocket events: ${serverEvents.join(",")}`);
    }
    console.log("[Hono Test 3.4] Server WebSocket lifecycle verified:", serverEvents.join(" -> "));

    server.stop();
    console.log("\n==========================================");
    console.log("===  ALL HONO + WEBSOCKET TESTS PASS!  ===");
    console.log("==========================================\n");
}

runTests().catch((err) => {
    console.error("Hono test failed:", err);
    server.stop();
    throw err;
});
