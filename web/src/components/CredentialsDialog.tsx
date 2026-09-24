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
import { Skeleton } from '@/components/ui/misc'
import { SettingField } from '@/components/SettingField'
import { api } from '@/lib/api'
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
        toast.success(`${name}: salvo e testado — ${formatCount(teste.resultados)} resultados`)
        onClose()
      } else {
        setFailure(`Salvo, mas o teste falhou: ${teste.erro ?? 'motivo desconhecido'}`)
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
        <DialogTitle>Editar {name}</DialogTitle>
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
            <SettingField key={setting.name} setting={setting} value={values[setting.name] ?? ''} onChange={set} />
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
