import { Tv } from 'lucide-react'
import { useState } from 'react'
import { cn } from '@/lib/utils'

export function SeriesPoster({
  poster,
  title,
  className,
}: {
  poster: string | null
  title: string
  className?: string
}) {
  const [failed, setFailed] = useState(false)
  return (
    <div
      className={cn(
        'relative aspect-[2/3] overflow-hidden rounded-md border border-border bg-surface-raised',
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
        <div className="flex size-full flex-col items-center justify-center gap-2 p-3 text-center">
          <Tv className="size-6 text-content-subtle" aria-hidden="true" />
          <span className="line-clamp-3 text-xs text-content-muted">{title}</span>
        </div>
      )}
    </div>
  )
}
