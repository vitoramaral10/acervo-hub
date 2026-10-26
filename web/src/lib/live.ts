// Tempo real: um `EventSource` para o app inteiro. Cada tópico que o servidor
// avisa invalida as consultas daquela tela (`TOPIC_KEYS`); ao reconectar depois
// de uma queda, invalida tudo, porque eventos podem ter se perdido.

import { useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { TOPIC_KEYS, api, isUnauthorized } from '@/lib/api'

export type LiveStatus = 'live' | 'reconnecting'

const EVENTS_URL = '/ui/api/eventos'
/** Erros seguidos que justificam uma chamada de teste: pode ser a sessão do Access. */
const PROBE_AFTER = 3
const RETRY_MS = 2_000
const RETRY_MAX_MS = 30_000

/** Abre o canal enquanto `enabled` (há sessão). */
export function useLive(enabled: boolean): LiveStatus {
  const queryClient = useQueryClient()
  const [status, setStatus] = useState<LiveStatus>('reconnecting')

  useEffect(() => {
    if (!enabled) return
    let source: EventSource | null = null
    let timer: ReturnType<typeof setTimeout> | undefined
    let errors = 0
    let lost = false
    let closed = false

    const probe = () => {
      // O `request()` trata a sessão do Access expirada (recarrega a página).
      api.session().catch((error: unknown) => {
        if (isUnauthorized(error)) void queryClient.invalidateQueries({ queryKey: ['sessao'] })
      })
    }

    const connect = () => {
      if (closed) return
      const next = new EventSource(EVENTS_URL)
      source = next
      next.addEventListener('ola', () => {
        errors = 0
        setStatus('live')
        if (lost) {
          lost = false
          void queryClient.invalidateQueries()
        }
      })
      next.addEventListener('mudanca', (event) => {
        let topics: unknown
        try {
          topics = (JSON.parse((event as MessageEvent<string>).data) as { topicos?: unknown }).topicos
        } catch {
          return
        }
        if (!Array.isArray(topics)) return
        for (const topic of topics) {
          for (const queryKey of TOPIC_KEYS[String(topic)] ?? [])
            void queryClient.invalidateQueries({ queryKey: [...queryKey] })
        }
      })
      next.onerror = () => {
        lost = true
        errors += 1
        setStatus('reconnecting')
        if (errors % PROBE_AFTER === 0) probe()
        // Resposta que não é um fluxo (o login do Access, um 401) fecha de vez o
        // `EventSource`: não há reconexão automática, então ela é nossa.
        if (next.readyState === EventSource.CLOSED) {
          next.close()
          timer = setTimeout(connect, Math.min(RETRY_MAX_MS, RETRY_MS * errors))
        }
      }
    }

    connect()
    return () => {
      closed = true
      clearTimeout(timer)
      source?.close()
    }
  }, [enabled, queryClient])

  return enabled ? status : 'reconnecting'
}
