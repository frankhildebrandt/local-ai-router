import { useMemo, useState } from "react";
import {
  AssistantRuntimeProvider,
  ComposerPrimitive,
  MessagePrimitive,
  ThreadPrimitive,
  useLocalRuntime,
  type ChatModelAdapter,
  type ThreadMessage,
} from "@assistant-ui/react";
import { Bot, RotateCcw, Send, Sparkles, User } from "lucide-react";
import { command } from "./api";
import type { ModelRoute } from "./types";

type ClientChatResponse = { content: string; model: string };

function messageText(message: ThreadMessage): string {
  return message.content
    .filter((part): part is Extract<(typeof message.content)[number], { type: "text" }> => part.type === "text")
    .map(part => part.text)
    .join("\n");
}

function ChatRuntime({ model }: { model: string }) {
  const sessionId = useMemo(() => crypto.randomUUID(), []);
  const adapter = useMemo<ChatModelAdapter>(() => ({
    async run({ messages, abortSignal }) {
      if (abortSignal.aborted) throw new DOMException("Chat request cancelled", "AbortError");
      const response = await command<ClientChatResponse>("client_chat", {
        input: {
          model,
          sessionId,
          messages: messages
            .filter(message => message.role !== "system" || messageText(message).trim())
            .map(message => ({ role: message.role, content: messageText(message) })),
        },
      });
      if (abortSignal.aborted) throw new DOMException("Chat request cancelled", "AbortError");
      return { content: [{ type: "text", text: response.content }] };
    },
  }), [model, sessionId]);
  const runtime = useLocalRuntime(adapter);

  return <AssistantRuntimeProvider runtime={runtime}>
    <ThreadPrimitive.Root className="chat-thread">
      <ThreadPrimitive.Viewport className="chat-viewport">
        <ThreadPrimitive.Empty>
          <div className="chat-empty">
            <div><Sparkles size={22} /></div>
            <h2>Start a private conversation</h2>
            <p>Messages use the selected alias and flow through your local router.</p>
          </div>
        </ThreadPrimitive.Empty>
        <ThreadPrimitive.Messages components={{ UserMessage, AssistantMessage }} />
        <div className="chat-composer-wrap">
          <ComposerPrimitive.Root className="chat-composer">
            <ComposerPrimitive.Input aria-label="Message" placeholder="Message your model…" rows={1} />
            <ComposerPrimitive.Send className="chat-send" aria-label="Send message"><Send size={16} /></ComposerPrimitive.Send>
          </ComposerPrimitive.Root>
          <small>Responses may be routed to cloud providers according to the alias configuration.</small>
        </div>
      </ThreadPrimitive.Viewport>
    </ThreadPrimitive.Root>
  </AssistantRuntimeProvider>;
}

function UserMessage() {
  return <MessagePrimitive.Root className="chat-message user-message">
    <div className="chat-avatar"><User size={15} /></div>
    <div className="chat-bubble"><MessagePrimitive.Content /></div>
  </MessagePrimitive.Root>;
}

function AssistantMessage() {
  return <MessagePrimitive.Root className="chat-message assistant-message">
    <div className="chat-avatar"><Bot size={16} /></div>
    <div className="chat-bubble"><MessagePrimitive.Content /><MessagePrimitive.Error><p className="chat-error">The model request failed. Check the alias and request logs for details.</p></MessagePrimitive.Error></div>
  </MessagePrimitive.Root>;
}

export function ChatPage({ routes }: { routes: ModelRoute[] }) {
  const available = routes.filter(route => route.enabled && route.capabilities.includes("chat"));
  const [selected, setSelected] = useState(available[0]?.alias ?? "");
  const [session, setSession] = useState(0);
  const model = available.some(route => route.alias === selected) ? selected : available[0]?.alias ?? "";

  if (!model) return <>
    <div className="page-head"><div><span className="eyebrow">Playground</span><h1>Chat</h1><p>An open-source chat interface, preconfigured for your local gateway.</p></div></div>
    <div className="chat-unavailable"><Bot size={25} /><h2>No chat alias available</h2><p>Create an enabled alias with the chat capability before starting a conversation.</p></div>
  </>;

  return <div className="chat-page">
    <div className="chat-toolbar">
      <div><span className="eyebrow">Playground</span><h1>Chat</h1><p>Powered by assistant-ui · routed privately through localhost</p></div>
      <div className="chat-controls">
        <label><span>Model alias</span><select aria-label="Model alias" value={model} onChange={event => { setSelected(event.target.value); setSession(value => value + 1); }}>{available.map(route => <option key={route.alias} value={route.alias}>{route.alias}</option>)}</select></label>
        <button className="secondary" onClick={() => setSession(value => value + 1)}><RotateCcw size={15} />New chat</button>
      </div>
    </div>
    <ChatRuntime key={`${model}-${session}`} model={model} />
  </div>;
}
