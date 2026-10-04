import { useMutation } from '@tanstack/react-query'
import { CircleAlert, CircleCheck } from 'lucide-react'
import { type ReactNode, useEffect, useRef } from 'react'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/misc'

/** O que um passo do plano mostra depois de aplicado: feito ou o erro. */
export function StepResult({ applied, done, error }: { applied: boolean; done: boolean; error: string | null }) {
  if (!applied) return null
  if (error)
    return (
      <p className="mt-1 flex items-start gap-1 text-xs text-danger">
        <CircleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
        {error}
      </p>
    )
  return done ? (
    <p className="mt-1 flex items-center gap-1 text-xs text-success">
      <CircleCheck className="size-3.5 shrink-0" aria-hidden="true" />
      Feito
    </p>
  ) : null
}

export function PlanSection({ title, count, children }: { title: string; count: number; children: ReactNode }) {
  return (
    <section>
      <h3 className="mb-1.5 text-sm font-medium">
        {title} <span className="font-normal text-content-subtle tabular-nums">({count})</span>
      </h3>
      <ul className="divide-y divide-border rounded-md border border-border">{children}</ul>
    </section>
  )
}

/**
 * Fluxo de duas etapas: simula ao abrir (`aplicar=false`), mostra o plano, aplica ao confirmar
 * (`aplicar=true`) e mostra o resultado. Monte só enquanto aberto: o estado nasce a cada abertura.
 */
export function PlanDialog<T extends { aplicado: boolean }>({
  title,
  description,
  run,
  isEmpty,
  emptyMessage,
  canApply,
  applyLabel,
  onApplied,
  onClose,
  children,
  summary,
}: {
  title: string
  description: string
  run: (aplicar: boolean) => Promise<T>
  /** Nada a fazer nem a mostrar. */
  isEmpty: (report: T) => boolean
  emptyMessage: string
  /** Há algo que a confirmação mude; sem isso só se mostra o plano. */
  canApply: (report: T) => boolean
  applyLabel: string
  onApplied: () => void
  onClose: () => void
  children: (report: T) => ReactNode
  summary: (report: T) => string
}) {
  const plan = useMutation({ mutationFn: run })
  const apply = useMutation({ mutationFn: () => run(true), onSuccess: onApplied })
  const started = useRef(false)
  const simulate = plan.mutate
  useEffect(() => {
    if (started.current) return
    started.current = true
    simulate(false)
  }, [simulate])

  const report = apply.data ?? plan.data
  const error = apply.error ?? plan.error
  const applied = report?.aplicado === true

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{applied ? 'Resultado da aplicação.' : description}</DialogDescription>
        </DialogHeader>

        {error && !apply.isPending ? (
          <div role="alert" className="grid gap-2">
            <p className="text-sm text-danger">{error.message}</p>
            <div>
              <Button
                onClick={() => {
                  apply.reset()
                  plan.mutate(false)
                }}
              >
                Tentar novamente
              </Button>
            </div>
          </div>
        ) : !report ? (
          <div className="grid gap-2" aria-busy="true">
            <span className="sr-only">Lendo o plano…</span>
            <Skeleton className="h-10" />
            <Skeleton className="h-10" />
            <Skeleton className="h-10" />
          </div>
        ) : isEmpty(report) ? (
          <p className="rounded-md border border-dashed border-border-strong px-4 py-8 text-center text-sm text-content-muted">
            {emptyMessage}
          </p>
        ) : (
          <div className="grid gap-4">
            {applied && (
              <p role="status" className="text-sm font-medium">
                {summary(report)}
              </p>
            )}
            {children(report)}
          </div>
        )}

        <DialogFooter>
          {report && !applied && !isEmpty(report) && canApply(report) ? (
            <>
              <Button variant="ghost" onClick={onClose}>
                Cancelar
              </Button>
              <Button variant="primary" loading={apply.isPending} onClick={() => apply.mutate()}>
                {applyLabel}
              </Button>
            </>
          ) : (
            <Button onClick={onClose}>Fechar</Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
