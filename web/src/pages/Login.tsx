import { useMutation } from '@tanstack/react-query'
import { KeyRound } from 'lucide-react'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { api } from '@/lib/api'

export function LoginPage({ onLoggedIn }: { onLoggedIn: () => void }) {
  const [key, setKey] = useState('')
  const [shownError, setShownError] = useState<string | null>(null)
  const login = useMutation({
    mutationFn: api.login,
    onSuccess: onLoggedIn,
    onError: (error: Error) => setShownError(error.message),
  })

  return (
    <main className="grid min-h-dvh place-items-center bg-bg px-4">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex flex-col items-center gap-3 text-center">
          <img src="/ui/icone.svg" alt="" className="size-11" />
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">acervo-hub</h1>
            <p className="mt-1 text-sm text-content-muted">Indexadores do seu acervo de mídia.</p>
          </div>
        </div>
        <form
          noValidate
          className="grid gap-5 rounded-lg border border-border bg-surface p-6 shadow-sm"
          onSubmit={(event) => {
            event.preventDefault()
            if (!key.trim()) {
              setShownError('Informe a chave de API.')
              return
            }
            setShownError(null)
            login.mutate(key.trim())
          }}
        >
          <div className="grid gap-2">
            <Label htmlFor="chave">Chave de API</Label>
            <div className="relative">
              <KeyRound className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-content-subtle" aria-hidden="true" />
              <Input
                id="chave"
                type="password"
                autoComplete="current-password"
                autoFocus
                value={key}
                onChange={(event) => setKey(event.target.value)}
                aria-invalid={shownError ? true : undefined}
                aria-describedby="chave-ajuda chave-erro"
                className="pl-9"
              />
            </div>
            <p id="chave-ajuda" className="text-xs text-content-subtle">
              A mesma de <code className="font-mono">server.api_key</code> no <code className="font-mono">config.toml</code>.
            </p>
            {shownError && (
              <p id="chave-erro" role="alert" className="text-sm text-danger">
                {shownError}
              </p>
            )}
          </div>
          <Button type="submit" variant="primary" size="lg" loading={login.isPending} className="w-full">
            Entrar
          </Button>
        </form>
      </div>
    </main>
  )
}
