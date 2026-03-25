{{/* Expand the name of the chart. */}}
{{- define "rustjack.name" -}}
rustjack-cainjector
{{- end }}

{{/* Create a default fully qualified app name. */}}
{{- define "rustjack.fullname" -}}
rustjack-cainjector
{{- end }}

{{/* Create the name of the service account to use */}}
{{- define "rustjack.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "rustjack.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end }}