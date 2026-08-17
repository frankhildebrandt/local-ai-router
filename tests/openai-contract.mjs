import assert from "node:assert/strict";
import OpenAI from "openai";

const client = new OpenAI({ baseURL: "http://127.0.0.1:11436/v1", apiKey: "contract-token" });
const models = await client.models.list();
assert.deepEqual(models.data.map(model => model.id), ["sdk-model"]);

const chat = await client.chat.completions.create({ model: "sdk-model", messages: [{ role: "user", content: "hello" }] });
assert.equal(chat.choices[0].message.content, "hello");

let streamed = "";
const stream = await client.chat.completions.create({ model: "sdk-model", messages: [{ role: "user", content: "hello" }], stream: true });
for await (const chunk of stream) streamed += chunk.choices[0]?.delta.content ?? "";
assert.equal(streamed, "hello");

const response = await client.responses.create({ model: "sdk-model", input: "hello" });
assert.equal(response.output_text, "hello");

const embedding = await client.embeddings.create({ model: "sdk-model", input: "hello", encoding_format: "float" });
assert.deepEqual(embedding.data[0].embedding, [0.1, 0.2]);
