// Runs Tern's OpenCode plugin against a fake OpenCode and a fake hook endpoint.
// Started by `opencode_hooks`' tests: node harness.mjs plugin.mjs hook-file
import assert from "node:assert/strict"
import fs from "node:fs"
import http from "node:http"
import { pathToFileURL } from "node:url"

const [, , pluginPath, hookFile] = process.argv
const plugin = (await import(pathToFileURL(pluginPath))).default

/// A hook endpoint that keeps what it got; `answer(body)` says what it replies,
/// or `null` to drop the connection.
async function endpoint(answer = () => ({})) {
  const got = []
  const server = http.createServer((req, res) => {
    let text = ""
    req.on("data", (d) => (text += d))
    req.on("end", () => {
      const body = JSON.parse(text)
      const reply = answer(body)
      if (reply === null) return req.socket.destroy()
      got.push(body)
      res.setHeader("content-type", "application/json")
      res.end(JSON.stringify(reply))
    })
  })
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
  fs.writeFileSync(hookFile, JSON.stringify({ hook_url: `http://127.0.0.1:${server.address().port}/` }))
  return { got, close: () => server.close() }
}

/// Run the plugin over `events`, with OpenCode's calls being `calls`.
async function run(events, calls = {}) {
  const ctx = {
    session: { get: async () => ({ id: "s" }), synthetic: async () => {}, ...calls.session },
    model: { list: async () => [] },
    event: {
      subscribe: async function* () {
        yield* events
      },
    },
  }
  const stop = await plugin.setup(ctx)
  await stop()
}

const kinds = (got) => got.map((b) => b.type)
const scenarios = {
  async "only what Tern reads is posted"() {
    const hook = await endpoint()
    const data = { sessionID: "s" }
    await run(["session.reasoning.delta", "session.tool.input.delta", "session.compaction.delta", "session.text.ended"].map((type) => ({ type, data })))
    assert.deepEqual(kinds(hook.got), ["session.text.ended"])
    hook.close()
  },
  async "a form's parent is looked up by the form's session"() {
    const hook = await endpoint()
    const asked = []
    const get = async ({ sessionID }) => (asked.push(sessionID), { id: sessionID, parentID: "r" })
    await run([{ type: "form.created", data: { form: { sessionID: "c" } } }], { session: { get } })
    assert.deepEqual(asked, ["c"])
    assert.equal(hook.got[0].parent, "r")
    hook.close()
  },
  async "a created session says its parent itself"() {
    const hook = await endpoint()
    const get = async () => assert.fail("no lookup needed")
    const data = { sessionID: "c", parentID: "r" }
    await run([{ type: "session.created", data }, { type: "session.text.ended", data }], { session: { get } })
    assert.deepEqual(hook.got.map((b) => b.parent), ["r", "r"])
    hook.close()
  },
  async "a failed lookup reads as unknown and is asked again"() {
    const hook = await endpoint()
    let asked = 0
    const get = async () => (++asked === 1 ? Promise.reject(new Error("no")) : { id: "s", parentID: null })
    const event = { type: "session.text.ended", data: { sessionID: "s" } }
    await run([event, event], { session: { get } })
    assert.deepEqual(hook.got.map((b) => b.parent), ["unknown", null])
    hook.close()
  },
  async "an answer that is no session is a failed lookup"() {
    const hook = await endpoint()
    const get = async () => ({ error: { name: "NotFound" } })
    await run([{ type: "session.text.ended", data: { sessionID: "s" } }], { session: { get } })
    assert.equal(hook.got[0].parent, "unknown")
    hook.close()
  },
  async "a call that throws doesn't end the forwarding"() {
    const hook = await endpoint(() => ({ steer: { sessionID: "s", text: "x" } }))
    const session = {
      get() {
        throw new Error("thrown, not rejected")
      },
      synthetic() {
        throw new Error("thrown, not rejected")
      },
    }
    const event = { type: "session.text.ended", data: { sessionID: "s" } }
    await run([event, event], { session })
    assert.equal(hook.got.length, 2)
    hook.close()
  },
  async "a call that hangs doesn't stop the forwarding"() {
    const hook = await endpoint()
    const event = { type: "session.text.ended", data: { sessionID: "s" } }
    await run([event, event], { session: { get: () => new Promise(() => {}) } })
    assert.equal(hook.got.length, 2)
    hook.close()
  },
  async "past a failed post only what lasts goes, until one is answered"() {
    let refuse = true
    const tried = []
    const hook = await endpoint((body) => {
      tried.push(body.type)
      if (body.type === "session.execution.succeeded") refuse = false
      return refuse ? null : {}
    })
    const data = { sessionID: "s" }
    const events = ["session.text.ended", "session.tool.called", "session.step.ended", "session.execution.succeeded", "session.text.ended"]
    await run(events.map((type) => ({ type, data })))
    assert.deepEqual(tried, ["session.text.ended", "session.execution.succeeded", "session.text.ended"])
    hook.close()
  },
  async "an answer that isn't JSON still is an answer"() {
    const server = http.createServer((req, res) => {
      req.resume()
      req.on("end", () => {
        posted += 1
        res.end("")
      })
    })
    let posted = 0
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
    fs.writeFileSync(hookFile, JSON.stringify({ hook_url: `http://127.0.0.1:${server.address().port}/` }))
    const event = { type: "session.text.ended", data: { sessionID: "s" } }
    await run([event, event])
    assert.equal(posted, 2)
    server.close()
  },
}

process.env.TERN_OPENCODE_HOOK_FILE = hookFile
let failed = 0
for (const [name, scenario] of Object.entries(scenarios)) {
  try {
    await Promise.race([scenario(), new Promise((_, no) => setTimeout(() => no(new Error("timed out")), 6000))])
  } catch (e) {
    failed += 1
    console.error(`FAILED ${name}: ${e.message.split("\n")[0]}`)
  }
}
process.exit(failed ? 1 : 0)
