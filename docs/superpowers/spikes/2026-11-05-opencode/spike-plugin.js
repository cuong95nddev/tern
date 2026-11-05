import fs from "fs"
const LOG = process.env.SPIKE_LOG || "/dev/null"
const log = (o) => fs.appendFileSync(LOG, JSON.stringify({ t: Date.now(), pid: process.pid, tab: process.env.SPIKE_TAB, ...o }) + "\n")
const shape = (o, d = 0) => {
  if (!o || (typeof o !== "object" && typeof o !== "function") || d > 2) return typeof o
  const out = {}
  for (const k of Object.keys(o)) out[k] = shape(o[k], d + 1)
  return out
}
let steered = false
async function setup(ctx) {
  log({ loaded: true })
  try { const m = await ctx.model.list(); log({ models: JSON.stringify(m).slice(0, 900) }) } catch (e) { log({ modelerr: String(e) }) }
  try { const d = await ctx.model.default(); log({ dflt: JSON.stringify(d).slice(0, 500) }) } catch (e) { log({ dflterr: String(e) }) }
  const c = new AbortController()
  ;(async () => {
    for await (const input of ctx.event.subscribe({ signal: c.signal })) {
      log({ e: input.type, p: input.data })
      if (input.type === "session.execution.succeeded" && process.env.SPIKE_STEER && !steered) {
        steered = true
        const sessionID = input.data.sessionID
        try { log({ ctxr: await ctx.session.context({ sessionID }) }) } catch (e) { log({ ctxerr: String(e) }) }
        try { log({ steer: await ctx.session.synthetic({ sessionID, text: process.env.SPIKE_STEER, resume: true }) }) } catch (e) { log({ steererr: String(e), stack: String(e?.stack).slice(0, 400) }) }
      }
    }
  })().catch((e) => log({ err: String(e) }))
  return async () => c.abort()
}
export default { id: "tern.spike", setup }
