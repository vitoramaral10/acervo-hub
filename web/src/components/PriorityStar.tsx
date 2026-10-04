import { Star } from 'lucide-react'
import { Badge } from '@/components/ui/misc'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

/** Estrela de marcar e desmarcar como prioritário, nos detalhes de filme e de série. */
export function PriorityStar({
  active,
  busy,
  onToggle,
}: {
  active: boolean
  busy?: boolean
  onToggle: (next: boolean) => void
}) {
  return (
    <Button
      variant="ghost"
      aria-pressed={active}
      disabled={busy}
      title={active ? 'Tirar a prioridade' : 'Marcar como prioritário'}
      onClick={() => onToggle(!active)}
    >
      <Star className={cn(active && 'fill-warning text-warning')} aria-hidden="true" />
      {active ? 'Prioritário' : 'Priorizar'}
    </Button>
  )
}

/** Selo no canto do pôster; some quando o item não é prioritário. */
export function PriorityBadge({ className }: { className?: string }) {
  return (
    <Badge tone="warning" className={cn('shadow-sm', className)} title="Prioritário">
      <Star className="fill-current" aria-hidden="true" />
      <span className="sr-only">Prioritário</span>
    </Badge>
  )
}
