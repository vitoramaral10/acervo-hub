import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Trash2, Undo2 } from 'lucide-react'
import { useMemo } from 'react'
import { toast } from 'sonner'
import { Button, type ButtonProps } from '@/components/ui/button'
import { Badge } from '@/components/ui/misc'
import { type DeletionTarget, deletionApi } from '@/lib/api'
import { cn } from '@/lib/utils'

export const DELETION_KEY = ['para-apagar'] as const

/** A chave de um alvo, a mesma que o servidor devolve em `chave`. */
export function targetKey(target: DeletionTarget): string {
  if ('filme' in target) return `filme-${target.filme}`
  return target.temporada === undefined ? `serie-${target.serie}` : `serie-${target.serie}-t${target.temporada}`
}

/** As chaves marcadas para apagar; a lista é pequena e serve às telas de filme, de série e à "Para apagar". */
export function useMarkedKeys(): Set<string> {
  const marks = useQuery({ queryKey: DELETION_KEY, queryFn: deletionApi.list })
  return useMemo(() => new Set(marks.data?.itens.map((item) => item.chave)), [marks.data])
}

/** Marca ou desmarca, e avisa as telas que mostram a marca. */
export function useToggleMark() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ targets, mark }: { targets: DeletionTarget[]; mark: boolean; label?: string }) => {
      if (mark) await deletionApi.mark(targets)
      else await deletionApi.unmark(targets)
    },
    onSuccess: (_, { mark, label }) =>
      toast.success(mark ? `Marcado para apagar: ${label ?? 'item'}` : `Desmarcado: ${label ?? 'item'}`),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: DELETION_KEY })
    },
  })
}

/** "Marcar para apagar" / "Desmarcar": só marca; apagar é na tela "Para apagar", com confirmação. */
export function MarkButton({
  target,
  marked,
  label,
  children,
  className,
  ...props
}: Omit<ButtonProps, 'onClick'> & { target: DeletionTarget; marked: boolean; label?: string }) {
  const toggle = useToggleMark()
  const busy = toggle.isPending
  return (
    <Button
      variant="ghost"
      aria-pressed={marked}
      loading={busy}
      title={marked ? 'Tirar da lista "Para apagar"' : 'Pôr na lista "Para apagar"; nada sai antes de confirmar lá'}
      className={cn(marked && 'text-danger', className)}
      onClick={() => toggle.mutate({ targets: [target], mark: !marked, label })}
      {...props}
    >
      {!busy && (marked ? <Undo2 aria-hidden="true" /> : <Trash2 aria-hidden="true" />)}
      {children ?? (marked ? 'Desmarcar' : 'Marcar para apagar')}
    </Button>
  )
}

/** Selo de "marcado para apagar", nos detalhes e nas capas. */
export function MarkedBadge({ className, label = 'Para apagar' }: { className?: string; label?: string }) {
  return (
    <Badge tone="danger" className={cn('shadow-sm', className)} title="Marcado para apagar">
      <Trash2 aria-hidden="true" />
      {label}
    </Badge>
  )
}
