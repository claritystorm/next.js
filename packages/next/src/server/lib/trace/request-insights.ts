import type { AttributeValue } from 'next/dist/compiled/@opentelemetry/api'
import type {
  RequestInsight,
  RequestInsightFetch,
  RequestInsightsSnapshot,
} from '../../../next-devtools/shared/request-insights'
import type { SpanStoreRecord } from './span-store'
export { isRequestInsightsEnabled } from './span-store'

const MAX_REQUEST_INSIGHTS = 100
const REQUEST_INSIGHTS_STORE_KEY = Symbol.for('@next/request-insights-store')

type RequestInsightsListener = (insight: RequestInsight) => void
type RequestInsightIdentity = {
  requestId?: string
  htmlRequestId?: string
  route?: string
  url?: string
}

class InMemoryRequestInsightsStore {
  private readonly requests = new Map<string, RequestInsight>()
  private readonly requestOrder: string[] = []
  private readonly listeners = new Set<RequestInsightsListener>()

  recordSpan(span: SpanStoreRecord): void {
    if (!span.requestId) {
      return
    }

    const insight = this.getOrCreateRequest(
      span,
      span.startTime ?? span.timestamp
    )

    const spanStartTime = span.startTime ?? span.timestamp
    const spanEndTime = span.durationMs
      ? spanStartTime + span.durationMs
      : spanStartTime
    const requestEndTime = insight.durationMs
      ? insight.startTime + insight.durationMs
      : insight.startTime

    insight.htmlRequestId = span.htmlRequestId ?? insight.htmlRequestId
    insight.route = insight.route ?? span.route
    insight.url = insight.url ?? span.url
    insight.startTime = Math.min(insight.startTime, spanStartTime)
    insight.durationMs =
      Math.max(requestEndTime, spanEndTime) - insight.startTime
    insight.status =
      insight.status === 'error' || span.status === 'error'
        ? 'error'
        : span.status === 'ok'
          ? 'ok'
          : insight.status

    insight.spans.push({
      name: span.name,
      startTime: spanStartTime,
      durationMs: span.durationMs,
      status: span.status,
      traceId: span.traceId,
      spanId: span.spanId,
      parentSpanId: span.parentSpanId,
      attributes: span.attributes,
      links: span.links,
      events: span.events,
      error: span.error,
    })

    const fetch = getFetchInsight(span)
    if (fetch) {
      this.recordFetchForInsight(insight, fetch)
    }

    this.notify(insight)
  }

  recordFetch(identity: RequestInsightIdentity, fetch: RequestInsightFetch) {
    if (!identity.requestId) {
      return
    }

    const fetchStartTime = fetch.startTime ?? Date.now()
    const insight = this.getOrCreateRequest(identity, fetchStartTime)
    const fetchEndTime = fetch.durationMs
      ? fetchStartTime + fetch.durationMs
      : fetchStartTime
    const requestEndTime = insight.durationMs
      ? insight.startTime + insight.durationMs
      : insight.startTime

    insight.durationMs =
      Math.max(requestEndTime, fetchEndTime) - insight.startTime
    this.recordFetchForInsight(insight, fetch)
    this.notify(insight)
  }

  getSnapshot(): RequestInsightsSnapshot {
    return {
      requests: this.requestOrder
        .map((requestId) => this.requests.get(requestId))
        .filter((request): request is RequestInsight => request !== undefined),
    }
  }

  subscribe(listener: RequestInsightsListener): () => void {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  clear(): void {
    this.requests.clear()
    this.requestOrder.length = 0
  }

  private notify(insight: RequestInsight): void {
    for (const listener of this.listeners) {
      listener(insight)
    }
  }

  private getOrCreateRequest(
    identity: RequestInsightIdentity,
    startTime: number
  ): RequestInsight {
    const requestId = identity.requestId!
    let insight = this.requests.get(requestId)

    if (!insight) {
      insight = {
        requestId,
        htmlRequestId: identity.htmlRequestId ?? requestId,
        route: identity.route,
        url: identity.url,
        startTime,
        status: 'pending',
        spans: [],
        fetches: [],
      }
      this.requests.set(requestId, insight)
      this.requestOrder.push(requestId)
      this.trim()
    }

    insight.htmlRequestId = identity.htmlRequestId ?? insight.htmlRequestId
    insight.route = insight.route ?? identity.route
    insight.url = insight.url ?? identity.url
    insight.startTime = Math.min(insight.startTime, startTime)

    return insight
  }

  private recordFetchForInsight(
    insight: RequestInsight,
    fetch: RequestInsightFetch
  ): void {
    if (
      insight.fetches.some(
        (existingFetch) =>
          existingFetch.url === fetch.url &&
          (existingFetch.index !== undefined && fetch.index !== undefined
            ? existingFetch.index === fetch.index
            : existingFetch.startTime === fetch.startTime)
      )
    ) {
      return
    }

    insight.fetches.push(fetch)
  }

  private trim(): void {
    while (this.requestOrder.length > MAX_REQUEST_INSIGHTS) {
      const requestId = this.requestOrder.shift()
      if (requestId) {
        this.requests.delete(requestId)
      }
    }
  }
}

export function recordRequestInsightSpan(span: SpanStoreRecord): void {
  getRequestInsightsStore().recordSpan(span)
}

export function recordRequestInsightFetch(
  identity: RequestInsightIdentity,
  fetch: RequestInsightFetch
): void {
  getRequestInsightsStore().recordFetch(identity, fetch)
}

export function getRequestInsightsSnapshot(): RequestInsightsSnapshot {
  return getRequestInsightsStore().getSnapshot()
}

export function subscribeRequestInsights(
  listener: RequestInsightsListener
): () => void {
  return getRequestInsightsStore().subscribe(listener)
}

export function clearRequestInsightsForTest(): void {
  getRequestInsightsStore().clear()
}

function getRequestInsightsStore(): InMemoryRequestInsightsStore {
  const globalStore = globalThis as typeof globalThis & {
    [REQUEST_INSIGHTS_STORE_KEY]?: InMemoryRequestInsightsStore
  }

  return (globalStore[REQUEST_INSIGHTS_STORE_KEY] ??=
    new InMemoryRequestInsightsStore())
}

function getFetchInsight(span: SpanStoreRecord): RequestInsightFetch | null {
  const attributes = span.attributes

  if (!attributes || attributes['next.span_type'] !== 'AppRender.fetch') {
    return null
  }

  return {
    url: getStringAttribute(attributes['http.url']) ?? span.url,
    method: getStringAttribute(attributes['http.method']),
    statusCode: getNumberAttribute(attributes['http.status_code']),
    startTime: span.startTime ?? span.timestamp,
    durationMs: span.durationMs,
    cacheStatus: getStringAttribute(attributes['next.fetch.cache_status']),
    cacheReason: getStringAttribute(attributes['next.fetch.cache_reason']),
    index: getNumberAttribute(attributes['next.fetch.idx']),
  }
}

function getStringAttribute(value: AttributeValue | undefined) {
  return typeof value === 'string' ? value : undefined
}

function getNumberAttribute(value: AttributeValue | undefined) {
  return typeof value === 'number' ? value : undefined
}
