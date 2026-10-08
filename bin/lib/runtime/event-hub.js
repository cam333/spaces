'use strict';

const RING_CAPACITY = 512;

/**
 * Sequence-numbered event ring. Events are { seq, type, payload, ts }.
 * A subscriber that throws never breaks emit for other subscribers.
 */
class EventHub {
  constructor(capacity = RING_CAPACITY) {
    this._capacity = capacity;
    this._buffer = [];
    this._nextSeq = 1;
    this._listeners = new Set();
  }

  get lastSeq() {
    return this._nextSeq - 1;
  }

  emit(type, payload) {
    const event = { seq: this._nextSeq++, type, payload, ts: Date.now() };
    this._buffer.push(event);
    if (this._buffer.length > this._capacity) this._buffer.shift();
    for (const fn of this._listeners) {
      try { fn(event); } catch { /* listener errors must not break emit */ }
    }
    return event.seq;
  }

  /**
   * Events with seq > afterSeq. gap=true means afterSeq predates the ring
   * and the caller must re-snapshot instead of applying deltas.
   */
  eventsAfter(afterSeq) {
    const oldest = this._buffer.length ? this._buffer[0].seq : this._nextSeq;
    const gap = afterSeq + 1 < oldest;
    return { gap, events: this._buffer.filter(e => e.seq > afterSeq) };
  }

  subscribe(fn) {
    this._listeners.add(fn);
    return () => this._listeners.delete(fn);
  }
}

module.exports = { EventHub, RING_CAPACITY };
