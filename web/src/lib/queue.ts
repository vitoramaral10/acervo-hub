import type { QueueItem } from '@/lib/api'

type QueueTone = 'success' | 'neutral' | 'warning' | 'danger'

interface ClientState {
  label: string
  tone: QueueTone
  /** Parado por algo que pede olhar: sem fonte, pausado, em espera ou com erro. */
  stuck: boolean
}

/** Estados do qBittorrent, em português. O que não está aqui aparece como veio. */
const CLIENT_STATES: Record<string, ClientState> = {
  downloading: { label: 'Baixando', tone: 'success', stuck: false },
  forcedDL: { label: 'Baixando (forçado)', tone: 'success', stuck: false },
  metaDL: { label: 'Buscando metadados', tone: 'neutral', stuck: false },
  forcedMetaDL: { label: 'Buscando metadados', tone: 'neutral', stuck: false },
  allocating: { label: 'Alocando espaço', tone: 'neutral', stuck: false },
  checkingDL: { label: 'Verificando', tone: 'neutral', stuck: false },
  checkingUP: { label: 'Verificando', tone: 'neutral', stuck: false },
  checkingResumeData: { label: 'Verificando', tone: 'neutral', stuck: false },
  moving: { label: 'Movendo arquivos', tone: 'neutral', stuck: false },
  uploading: { label: 'Baixado, semeando', tone: 'success', stuck: false },
  forcedUP: { label: 'Baixado, semeando', tone: 'success', stuck: false },
  stalledUP: { label: 'Baixado, semeando', tone: 'success', stuck: false },
  queuedUP: { label: 'Baixado, na fila do cliente', tone: 'neutral', stuck: false },
  pausedUP: { label: 'Baixado, parado', tone: 'neutral', stuck: false },
  stoppedUP: { label: 'Baixado, parado', tone: 'neutral', stuck: false },
  stalledDL: { label: 'Sem fontes', tone: 'warning', stuck: true },
  queuedDL: { label: 'Na fila do cliente', tone: 'warning', stuck: true },
  pausedDL: { label: 'Pausado', tone: 'warning', stuck: true },
  stoppedDL: { label: 'Pausado', tone: 'warning', stuck: true },
  error: { label: 'Erro no cliente', tone: 'danger', stuck: true },
  missingFiles: { label: 'Arquivos sumiram', tone: 'danger', stuck: true },
  unknown: { label: 'Estado desconhecido', tone: 'warning', stuck: true },
}

export interface QueueStatus {
  label: string
  tone: QueueTone
  stuck: boolean
}

/** O que a linha da fila diz do download: o estado no cliente, ou que o cliente não o conhece. */
export function queueStatus(item: QueueItem): QueueStatus {
  if (!item.no_cliente) return { label: 'Fora do cliente', tone: 'danger', stuck: true }
  if (item.estado_cliente == null) return { label: 'Aguardando o cliente', tone: 'neutral', stuck: false }
  return (
    CLIENT_STATES[item.estado_cliente] ?? { label: item.estado_cliente, tone: 'neutral', stuck: false }
  )
}

/** Travado: o cliente parou, não tem o torrent, ou a importação reclamou. */
export function isStuck(item: QueueItem): boolean {
  return queueStatus(item).stuck || Boolean(item.mensagem?.startsWith('importação'))
}

/** Os travados primeiro; dentro de cada grupo, a ordem que o servidor mandou. */
export function stuckFirst(items: QueueItem[]): QueueItem[] {
  return [...items.filter(isStuck), ...items.filter((item) => !isStuck(item))]
}
