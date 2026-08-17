import assert from "node:assert/strict";
import Anthropic from "@anthropic-ai/sdk";
import { GoogleGenAI } from "@google/genai";

const anthropic = new Anthropic({ baseURL: "http://127.0.0.1:11436", apiKey: "contract-token" });
const message = await anthropic.messages.create({ model: "sdk-model", max_tokens: 64, messages: [{ role: "user", content: "hello" }] });
assert.equal(message.content[0].text, "hello");

let anthropicStream = "";
const events = await anthropic.messages.create({ model: "sdk-model", max_tokens: 64, messages: [{ role: "user", content: "hello" }], stream: true });
for await (const event of events) if (event.type === "content_block_delta" && event.delta.type === "text_delta") anthropicStream += event.delta.text;
assert.equal(anthropicStream, "hello");

const anthropicTool = await anthropic.messages.create({ model: "sdk-model", max_tokens: 64, messages: [{ role: "user", content: "hello" }], tools: [{ name: "lookup", input_schema: { type: "object", properties: { query: { type: "string" } } } }] });
assert.equal(anthropicTool.content.find(block => block.type === "tool_use")?.name, "lookup");

const google = new GoogleGenAI({ apiKey: "contract-token", httpOptions: { baseUrl: "http://127.0.0.1:11436/v1beta", apiVersion: "" } });
const generated = await google.models.generateContent({ model: "sdk-model", contents: "hello" });
assert.equal(generated.text, "hello");

let geminiStream = "";
const chunks = await google.models.generateContentStream({ model: "sdk-model", contents: "hello" });
for await (const chunk of chunks) geminiStream += chunk.text ?? "";
assert.equal(geminiStream, "hello");

const geminiTool = await google.models.generateContent({ model: "sdk-model", contents: "hello", config: { tools: [{ functionDeclarations: [{ name: "lookup", parametersJsonSchema: { type: "object", properties: { query: { type: "string" } } } }] }] } });
assert.equal(geminiTool.functionCalls?.[0]?.name, "lookup");
