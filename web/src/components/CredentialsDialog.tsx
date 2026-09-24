import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleAlert } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type Setting, api } from '@/lib/api'
import { formatCount } from '@/lib/format'

export function CredentialsDialog({ name, onClose }: { name: string | null; onClose: () => void }) {
  return (
    <Dialog open={name !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        {name && <CredentialsForm key={name} name={name} onClose={onClose} />}
      </DialogContent>
    </Dialog>
  )
}

function CredentialsForm({ name, onClose }: { name: string; onClose: () => void }) {
  const queryClient = useQueryClient()
  const settings = useQuery({ queryKey: ['settings', name], queryFn: () => api.settings(name) })
  const [values, setValues] = useState<Record<string, string>>({})
  const [failure, setFailure] = useState<string | null>(null)

  // Valores iniciais: o atual quando não é segredo; segredo começa vazio.
  useEffect(() => {
    if (!settings.data) return
    setValues(
      Object.fromEntries(settings.data.settings.map((setting) => [setting.name, setting.secret ? '' : (setting.value ?? '')])),
    )
  }, [settings.data])

  const save = useMutation({
    mutationFn: () => api.saveSettings(name, values),
    onSuccess: ({ teste }) => {
      void queryClient.invalidateQueries({ queryKey: ['indexadores'] })
      void queryClient.invalidateQueries({ queryKey: ['settings', name] })
      if (teste.ok) {
        toast.success(`${name}: credenciais salvas e testadas — ${formatCount(teste.resultados)} resultados`)
        onClose()
      } else {
        setFailure(`Salvas, mas o teste falhou: ${teste.erro ?? 'motivo desconhecido'}`)
      }
    },
    onError: (error: Error) => setFailure(error.message),
  })

  const set = (key: string, value: string) => setValues((current) => ({ ...current, [key]: value }))

  return (
    <form
      noValidate
      className="grid gap-5"
      onSubmit={(event) => {
        event.preventDefault()
        setFailure(null)
        save.mutate()
      }}
    >
      <DialogHeader>
        <DialogTitle>Credenciais de {name}</DialogTitle>
        <DialogDescription>
          Campo secreto em branco mantém o valor atual. Ao salvar, o indexador é testado na hora.
        </DialogDescription>
      </DialogHeader>

      {settings.isPending ? (
        <div className="grid gap-4" aria-busy="true" aria-label="Carregando settings">
          {[0, 1, 2].map((key) => (
            <div key={key} className="grid gap-2">
              <Skeleton className="h-3.5 w-24" />
              <Skeleton className="h-9" />
            </div>
          ))}
        </div>
      ) : settings.isError ? (
        <p role="alert" className="rounded-md bg-danger-bg px-3 py-2 text-sm text-danger">
          Não foi possível carregar os settings: {settings.error.message}
        </p>
      ) : (
        <div className="grid gap-4">
          {settings.data.settings.map((setting) => (
            <Field key={setting.name} setting={setting} value={values[setting.name] ?? ''} onChange={set} />
          ))}
        </div>
      )}

      {failure && (
        <p role="alert" className="flex gap-2 rounded-md bg-danger-bg px-3 py-2 text-sm text-danger">
          <CircleAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          <span>{failure}</span>
        </p>
      )}

      <DialogFooter>
        <Button variant="ghost" onClick={onClose}>
          Cancelar
        </Button>
        <Button type="submit" variant="primary" loading={save.isPending} disabled={!settings.data}>
          {save.isPending ? 'Salvando e testando…' : 'Salvar e testar'}
        </Button>
      </DialogFooter>
    </form>
  )
}

function Field({
  setting,
  value,
  onChange,
}: {
  setting: Setting
  value: string
  onChange: (name: string, value: string) => void
}) {
  const id = `setting-${setting.name}`
  if (setting.kind === 'checkbox') {
    return (
      <div className="flex items-center justify-between gap-4 rounded-md border border-border px-3 py-2.5">
        <Label htmlFor={id} className="font-normal">
          {setting.label}
        </Label>
        <Switch id={id} checked={value === 'true'} onCheckedChange={(checked) => onChange(setting.name, String(checked))} />
      </div>
    )
  }
  if (setting.kind === 'select') {
    return (
      <div className="grid gap-2">
        <Label htmlFor={id}>{setting.label}</Label>
        <Select value={value} onValueChange={(next) => onChange(setting.name, next)}>
          <SelectTrigger id={id}>
            <SelectValue placeholder="Escolha…" />
          </SelectTrigger>
          <SelectContent>
            {setting.options.map((option) => (
              <SelectItem key={option} value={option}>
                {option}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    )
  }
  return (
    <div className="grid gap-2">
      <div className="flex items-baseline justify-between gap-2">
        <Label htmlFor={id}>{setting.label}</Label>
        {setting.secret && (
          <span className={setting.is_set ? 'text-xs text-success' : 'text-xs text-content-subtle'}>
            {setting.is_set ? 'definido' : 'não definido'}
          </span>
        )}
      </div>
      <Input
        id={id}
        type={setting.secret ? 'password' : 'text'}
        autoComplete={setting.secret ? 'new-password' : 'off'}
        spellCheck={false}
        value={value}
        placeholder={setting.secret && setting.is_set ? 'Deixe vazio para manter o atual' : undefined}
        onChange={(event) => onChange(setting.name, event.target.value)}
      />
    </div>
  )
}
