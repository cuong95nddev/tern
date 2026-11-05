// Tern's plugin in an OpenCode tab: it forwards OpenCode's events to the tab's
// hook endpoint and carries out the one thing an answer asks, a steer. What
// the events mean is worked out in Tern (`opencode_fold.rs`), where it is tested.
import fs from "node:fs"

const POST_TIMEOUT_MS = 2000
const FORWARDED = /^(session|permission|form)\./
// Too many, and nothing Tern reads.
const SKIPPED = new Set([
  "session.text.delta",
  "session.text.started",
  "session.step.streamed",
  "session.tool.progress",
  "session.tool.input.ended",
  "session.usage.updated",
])

function hookUrl() {
  try {
    return JSON.parse(fs.readFileSync(process.env.TERN_OPENCODE_HOOK_FILE, "utf8")).hook_url
  } catch {
    return undefined
  }
}

// A Tern that is gone or slow must never hold OpenCode up.
async function post(url, body) {
  try {
    const response = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(POST_TIMEOUT_MS),
    })
    return await response.json()
  } catch {
    return undefined
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

  async function parentOf(sessionID) {
    if (!parents.has(sessionID)) {
      const session = await Promise.resolve(ctx.session.get({ sessionID })).then(record).catch(() => undefined)
      parents.set(sessionID, session?.parentID ?? null)
    }
    return parents.get(sessionID)
  }

  async function windowOf(model) {
    const key = `${model?.providerID}/${model?.id}`
    if (!windows.has(key)) {
      const list = await Promise.resolve(ctx.model.list()).then(record).catch(() => [])
      const found = list.find?.((m) => m.providerID === model?.providerID && m.modelID === model?.id)
      if (found) windows.set(key, found.limit?.context)
    }
    return windows.get(key)
  }

  async function forward({ type, data }) {
    const body = { hook_event_name: "OpenCode", type, data }
    const sessionID = data?.sessionID
    if (type === "session.step.started") models.set(sessionID, data.model)
    if (type === "session.tool.success") body.data = { ...data, content: undefined }
    if (type === "session.step.ended") body.window = await windowOf(models.get(sessionID))
    if (sessionID) body.parent = await parentOf(sessionID)
    const answer = await post(url, body)
    if (answer?.steer) await Promise.resolve(ctx.session.synthetic(answer.steer)).catch(() => {})
  }

  const consuming = (async () => {
    for await (const input of ctx.event.subscribe({ signal: controller.signal })) {
      if (FORWARDED.test(input.type) && !SKIPPED.has(input.type)) await forward(input)
    }
  })().catch(() => {})

  return async () => {
    controller.abort()
    await consuming
  }
}

export default { id: "tern.opencode", setup }
