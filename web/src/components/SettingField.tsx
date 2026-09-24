import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import type { Setting } from '@/lib/api'

export function SettingField({
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
