import { Film, Tv } from 'lucide-react'
import { useState } from 'react'
import type { MediaKind } from '@/lib/api'
import { cn } from '@/lib/utils'

/** Pôster pequeno de lista; sem imagem (ou com ela quebrada), o ícone do tipo. */
export function MediaThumb({
  poster,
  kind,
  className,
}: {
  poster: string | null
  kind: MediaKind
  className?: string
}) {
  const [failed, setFailed] = useState(false)
  const Icon = kind === 'filme' ? Film : Tv
  return (
    <div
      className={cn(
        'grid aspect-[2/3] w-10 shrink-0 place-items-center overflow-hidden rounded-sm border border-border bg-surface-raised',
        className,
      )}
    >
      {poster && !failed ? (
        <img
          src={poster}
          alt=""
          loading="lazy"
          decoding="async"
          onError={() => setFailed(true)}
          className="size-full object-cover"
        />
      ) : (
        <Icon className="size-4 text-content-subtle" aria-hidden="true" />
      )}
    </div>
  )
}
