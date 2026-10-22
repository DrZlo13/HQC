import { render } from 'preact';
import { Window } from 'happy-dom';
import { ChatItems, StreamingBubble, type ChatActions } from '../src/ui/cards';
import type { ChatSession } from '../src/session';

/** Renders a chat's timeline into a detached happy-dom document (Preact creates nodes
 *  through the container's ownerDocument, so no DOM globals are needed). */
export function renderChatToElement(chat: ChatSession, overrides: Partial<ChatActions> = {}): HTMLElement {
  const win = new Window();
  const el = win.document.createElement('div') as unknown as HTMLElement;
  win.document.body.appendChild(el as never);
  const actions: ChatActions = {
    timeline: chat.timeline,
    approve: (r, o) => chat.approve(r, o),
    answer: (r, a) => chat.answer(r, a),
    resend: (op) => chat.resend(op),
    discard: (op) => chat.discard(op),
    openArtifact: () => {},
    ...overrides,
  };
  render(
    <>
      <ChatItems items={chat.timeline.items()} actions={actions} />
      {chat.timeline.streaming && <StreamingBubble text={chat.timeline.streaming} />}
    </>,
    el,
  );
  return el;
}
