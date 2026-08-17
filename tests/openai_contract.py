from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:11436/v1", api_key="contract-token")
assert [model.id for model in client.models.list().data] == ["sdk-model"]
assert client.chat.completions.create(
    model="sdk-model", messages=[{"role": "user", "content": "hello"}]
).choices[0].message.content == "hello"
assert client.responses.create(model="sdk-model", input="hello").output_text == "hello"
assert client.embeddings.create(model="sdk-model", input="hello", encoding_format="float").data[0].embedding == [0.1, 0.2]
