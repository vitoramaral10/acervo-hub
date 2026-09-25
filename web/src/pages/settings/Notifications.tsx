import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Bell, CircleCheck, CircleDashed, Send } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import { ConfirmButton } from '@/components/ConfirmButton'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { type GotifyInput, type NotifyOn, library } from '@/lib/api'

const EVENT_FIELDS: { key: keyof NotifyOn; label: string; description: string }[] = [
  { key: 'pegou', label: 'Pegou', description: 'Quando pegar um release' },
  { key: 'importou', label: 'Importou', description: 'Quando importar' },
  { key: 'atualizou', label: 'Atualizou', description: 'Quando trocar por versão melhor' },
  { key: 'falhou', label: 'Falhou', description: 'Quando um download falhar' },
  { key: 'removido', label: 'Removido', description: 'Quando um filme for removido' },
]

const DEFAULT_EVENTS: NotifyOn = { pegou: true, importou: true, atualizou: true, falhou: true, removido: false }

export function NotificationsSection() {
  const queryClient = useQueryClient()
  const notifications = useQuery({ queryKey: ['notificacoes'], queryFn: library.notifications })
  const gotify = notifications.data?.gotify ?? null

  const [servidor, setServidor] = useState('')
  const [token, setToken] = useState('')
  const [prioridade, setPrioridade] = useState(5)
  const [ligado, setLigado] = useState(false)
  const [eventos, setEventos] = useState<NotifyOn>(DEFAULT_EVENTS)
  const [error, setError] = useState<string | null>(null)
  const initialized = useRef(false)

  useEffect(() => {
    if (!notifications.data || initialized.current) return
    initialized.current = true
    const g = notifications.data.gotify
    if (g) {
      setServidor(g.servidor)
      setPrioridade(g.prioridade)
      setLigado(g.ligado)
      setEventos(g.eventos)
    }
  }, [notifications.data])

  function buildInput(): GotifyInput | null {
    if (!servidor.trim()) {
      setError('Preencha o endereço do servidor.')
      return null
    }
    if (!gotify?.token_definido && !token.trim()) {
      setError('Cole o token do aplicativo antes de salvar.')
      return null
    }
    setError(null)
    const input: GotifyInput = { servidor: servidor.trim(), prioridade, eventos, ligado }
    if (token.trim()) input.token = token.trim()
    return input
  }

  const test = useMutation({
    mutationFn: (value: GotifyInput) => library.testNotification(value),
    onSuccess: (result) =>
      result.ok ? toast.success('Notificação de teste enviada') : toast.error('O Gotify não aceitou o teste'),
    onError: (err: Error) => toast.error(err.message),
  })

  const save = useMutation({
    mutationFn: (value: GotifyInput | null) => library.saveNotifications(value),
    onSuccess: (data, value) => {
      queryClient.setQueryData(['notificacoes'], data)
      setToken('')
      toast.success(value === null ? 'Notificação removida' : 'Notificação salva')
    },
    onError: (err: Error) => toast.error(err.message),
  })

  const status = !gotify
    ? { label: 'Não configurado', tone: undefined, Icon: CircleDashed }
    : gotify.ligado
      ? { label: 'Ligado', tone: 'success' as const, Icon: CircleCheck }
      : { label: 'Desligado', tone: undefined, Icon: CircleDashed }

  return (
    <section aria-labelledby="gotify-titulo" className="max-w-2xl rounded-lg border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id="gotify-titulo" className="flex items-center gap-2 text-lg font-semibold">
            <Bell className="size-4.5 text-content-subtle" aria-hidden="true" />
            Gotify
          </h2>
          <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
            Avisa por push quando um release é pego, importado, trocado por versão melhor, falha ou é removido.
          </p>
        </div>
        {notifications.isPending ? (
          <Skeleton className="h-6 w-28" />
        ) : (
          <Badge tone={status.tone}>
            <status.Icon aria-hidden="true" />
            {status.label}
          </Badge>
        )}
      </div>

      {notifications.isError ? (
        <div role="alert" className="mt-6 flex flex-col items-start gap-3">
          <p className="text-sm text-danger">{notifications.error.message}</p>
          <Button onClick={() => void notifications.refetch()}>Tentar novamente</Button>
        </div>
      ) : notifications.isPending ? (
        <div aria-busy="true" className="mt-6 grid gap-4">
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-9 w-32" />
        </div>
      ) : (
        <form
          noValidate
          className="mt-6 grid gap-4"
          onSubmit={(event) => {
            event.preventDefault()
            const input = buildInput()
            if (input) save.mutate(input)
          }}
        >
          <div className="grid gap-2">
            <Label htmlFor="gotify-servidor">Servidor</Label>
            <Input
              id="gotify-servidor"
              type="url"
              autoComplete="off"
              spellCheck={false}
              value={servidor}
              onChange={(event) => setServidor(event.target.value)}
              placeholder="https://gotify.exemplo.com"
              aria-invalid={error ? true : undefined}
              aria-describedby="gotify-erro"
            />
          </div>

          <div className="grid gap-2">
            <Label htmlFor="gotify-token">Token do aplicativo</Label>
            <Input
              id="gotify-token"
              type="password"
              autoComplete="off"
              spellCheck={false}
              value={token}
              onChange={(event) => setToken(event.target.value)}
              placeholder={gotify?.token_definido ? 'Definido — cole outro para trocar' : ''}
              aria-invalid={error ? true : undefined}
              aria-describedby="gotify-erro"
              className="font-mono"
            />
          </div>

          <div className="grid gap-2">
            <Label htmlFor="gotify-prioridade">Prioridade</Label>
            <Input
              id="gotify-prioridade"
              type="number"
              min={0}
              max={10}
              step={1}
              value={prioridade}
              onChange={(event) => setPrioridade(Math.min(10, Math.max(0, Number(event.target.value) || 0)))}
              className="w-24 tabular-nums"
            />
          </div>

          {error && (
            <p id="gotify-erro" role="alert" className="text-sm text-danger">
              {error}
            </p>
          )}

          <div className="flex items-center justify-between gap-6 border-t border-border pt-4">
            <div>
              <p id="gotify-ligado-titulo" className="text-sm font-medium">
                Ligado
              </p>
              <p className="text-xs text-content-subtle">Sem isto, nenhum aviso sai, mesmo salvo.</p>
            </div>
            <Switch checked={ligado} onCheckedChange={setLigado} aria-labelledby="gotify-ligado-titulo" />
          </div>

          <div className="grid gap-3 border-t border-border pt-4">
            <p className="text-sm font-medium">Eventos</p>
            {EVENT_FIELDS.map(({ key, label, description }) => (
              <div key={key} className="flex items-center justify-between gap-6">
                <div>
                  <p id={`gotify-evento-${key}`} className="text-sm">
                    {label}
                  </p>
                  <p className="text-xs text-content-subtle">{description}</p>
                </div>
                <Switch
                  checked={eventos[key]}
                  onCheckedChange={(on) => setEventos((prev) => ({ ...prev, [key]: on }))}
                  aria-labelledby={`gotify-evento-${key}`}
                />
              </div>
            ))}
          </div>

          <div className="flex flex-wrap gap-2 border-t border-border pt-4">
            <Button
              type="button"
              variant="secondary"
              loading={test.isPending}
              onClick={() => {
                const input = buildInput()
                if (input) test.mutate(input)
              }}
            >
              {!test.isPending && <Send aria-hidden="true" />}
              Testar
            </Button>
            <Button type="submit" variant="primary" loading={save.isPending && save.variables !== null}>
              Salvar
            </Button>
            {gotify && (
              <ConfirmButton
                type="button"
                variant="ghost"
                loading={save.isPending && save.variables === null}
                title="Remover a notificação do Gotify?"
                confirmLabel="Remover"
                onConfirm={() => save.mutate(null)}
              >
                Remover
              </ConfirmButton>
            )}
          </div>
        </form>
      )}
    </section>
  )
}
