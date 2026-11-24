/// <reference lib="webworker" />
// Worker entry: instantiates the shared, already-compiled module and serves engine requests.
import { respond, type EngineRequest } from "./engine.js";
import { toPayload } from "./errors.js";
import { instantiate } from "./wasm.js";

declare const self: DedicatedWorkerGlobalScope;

type Message = { id: number; op: "init"; module: WebAssembly.Module } | ({ id: number } & EngineRequest);

self.onmessage = async (event: MessageEvent<Message>) => {
  const message = event.data;
  if (message.op === "init") {
    try {
      instantiate(message.module);
      self.postMessage({ id: message.id, ok: true, value: undefined });
    } catch (error) {
      self.postMessage({ id: message.id, ok: false, error: toPayload(error) });
    }
    return;
  }
  const { response, transfer } = await respond(message.id, message);
  self.postMessage(response, transfer);
};
