type EventCallback<T> = (event: { event: string; id: number; payload: T }) => void;
type EventListener = EventCallback<unknown>;

const listeners = new Map<string, Map<number, EventListener>>();
let nextListenerId = 1;

const dispatch = (event: string, payload: unknown): void => {
  const eventListeners = listeners.get(event);
  if (!eventListeners) {
    return;
  }

  for (const [id, listener] of [...eventListeners]) {
    listener({ event, id, payload });
  }
};

const runtime = window as Window & {
  __nwflash_wdio_emit_event__?: (event: string, payload: unknown) => void;
};
runtime.__nwflash_wdio_emit_event__ = dispatch;

// E2E builds provide the same listener lifecycle as Tauri events without native event injection.
export const listen = async <T>(event: string, handler: EventCallback<T>): Promise<() => void> => {
  const id = nextListenerId++;
  const eventListeners = listeners.get(event) ?? new Map<number, EventListener>();
  eventListeners.set(id, handler as EventListener);
  listeners.set(event, eventListeners);

  return () => {
    eventListeners.delete(id);
    if (eventListeners.size === 0) {
      listeners.delete(event);
    }
  };
};
