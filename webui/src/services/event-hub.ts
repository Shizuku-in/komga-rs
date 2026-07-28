export type EventHandler = (...args: any[]) => void

export default class EventHub {
  private readonly handlers = new Map<string, Set<EventHandler>>()

  $on(event: string, handler: EventHandler) {
    const listeners = this.handlers.get(event) ?? new Set<EventHandler>()
    listeners.add(handler)
    this.handlers.set(event, listeners)
  }

  $off(event: string, handler?: EventHandler) {
    if (handler) this.handlers.get(event)?.delete(handler)
    else this.handlers.delete(event)
  }

  $emit(event: string, ...args: any[]) {
    this.handlers.get(event)?.forEach(handler => handler(...args))
  }
}
