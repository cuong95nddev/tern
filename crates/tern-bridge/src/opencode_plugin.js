// Tern's plugin in an OpenCode tab: it forwards OpenCode's events to the tab's
// hook endpoint and carries out the one thing an answer asks, a steer. What
// the events mean is worked out in Tern (`opencode_fold.rs`), where it is tested.
import fs from "node:fs"

const POST_TIMEOUT_MS = 2000
const CALL_TIMEOUT_MS = 2000
// What Tern reads. The rest comes too often, a post each, for nothing.
const FORWARDED = new Set([
  "session.created",
  "session.viewed",
  "session.inbox.enqueued",
  "session.execution.started",
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
  "session.step.started",
  "session.step.ended",
  "session.tool.input.started",
  "session.tool.called",
  "session.tool.success",
  "session.tool.failed",
  "session.text.ended",
  "session.compaction.ended",
  "permission.asked",
  "permission.replied",
  "form.created",
  "form.replied",
  "form.cancelled",
])
// What a tab's state rests on: it goes even to a Tern that didn't answer the
// last post. The rest of a backlog would only leave Tern further behind.
const LASTING = new Set([
  "session.created",
  "session.inbox.enqueued",
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
  "session.compaction.ended",
  "permission.asked",
  "permission.replied",
  "form.created",
  "form.replied",
  "form.cancelled",
])

function hookUrl() {
  try {
    return JSON.parse(fs.readFileSync(process.env.TERN_OPENCODE_HOOK_FILE, "utf8")).hook_url
  } catch {
    return undefined
  }
}

// A Tern that is gone or slow must never hold OpenCode up. Whether it was
// reached, and what it answered.
async function post(url, body) {
  try {
    const response = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(POST_TIMEOUT_MS),
    })
    return { reached: true, answer: await response.json().catch(() => undefined) }
  } catch {
    return { reached: false }
  }
}

// One of OpenCode's calls: whatever it does, thrown or stuck, gives undefined
// and the forwarding goes on.
async function call(f) {
  let timer
  const late = new Promise((resolve) => {
    timer = setTimeout(resolve, CALL_TIMEOUT_MS)
  })
  try {
    return await Promise.race([(async () => f())(), late])
  } catch {
    return undefined
  } finally {
    clearTimeout(timer)
  }
}

// Calls answer with the record, or with it under `data`.
const record = (result) => result?.data ?? result

async function setup(ctx) {
  const noop = async () => {}
  const url = process.env.TERN_OPENCODE_HOOK_FILE && hookUrl()
  if (!url || typeof ctx?.event?.subscribe !== "function") return noop

  const controller = new AbortController()
  const parents = new Map()
  const models = new Map()
  const windows = new Map()

  // A failed lookup isn't kept, and isn't "no parent": only a session known
  // to be a root may take the tab.
  async function parentOf(sessionID) {
    if (!parents.has(sessionID)) {
      const session = record(await call(() => ctx.session.get({ sessionID })))
      if (typeof session?.id !== "string") return "unknown"
      parents.set(sessionID, session.parentID ?? null)
    }
    return parents.get(sessionID)
  }

  async function windowOf(model) {
    const key = `${model?.providerID}/${model?.id}`
    if (!windows.has(key)) {
      const list = record(await call(() => ctx.model.list())) ?? []
      const found = list.find?.((m) => m.providerID === model?.providerID && m.modelID === model?.id)
      if (found) windows.set(key, found.limit?.context)
    }
    return windows.get(key)
  }

  // Whether Tern was reached.
  async function forward({ type, data }) {
    const body = { hook_event_name: "OpenCode", type, data }
    const sessionID = data?.sessionID ?? data?.form?.sessionID
    if (type === "session.created") parents.set(sessionID, data.parentID ?? null)
    if (type === "session.tool.success") body.data = { ...data, content: undefined }
    if (type === "session.step.ended") body.window = await windowOf(models.get(sessionID))
    if (sessionID) body.parent = await parentOf(sessionID)
    const { reached, answer } = await post(url, body)
    if (answer?.steer) await call(() => ctx.session.synthetic(answer.steer))
    return reached
  }

  const consuming = (async () => {
    let reachable = true
    for await (const input of ctx.event.subscribe({ signal: controller.signal })) {
      if (!FORWARDED.has(input.type)) continue
      if (input.type === "session.step.started") models.set(input.data?.sessionID, input.data?.model)
      if (!reachable && !LASTING.has(input.type)) continue
      try {
        reachable = await forward(input)
      } catch {
        // One event lost; the next may go.
      }
    }
  })().catch(() => {})

  return async () => {
    controller.abort()
    await consuming
  }
}

export default { id: "tern.opencode", setup }
