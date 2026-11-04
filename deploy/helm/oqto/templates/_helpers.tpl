{{- define "oqto.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "oqto.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "oqto.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "oqto.labels" -}}
helm.sh/chart: {{ include "oqto.chart" . }}
app.kubernetes.io/name: {{ include "oqto.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "oqto.selectorLabels" -}}
app.kubernetes.io/name: {{ include "oqto.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "oqto.secretName" -}}
{{- default (printf "%s-env" (include "oqto.fullname" .)) .Values.secret.existingSecret -}}
{{- end -}}

{{- define "oqto.openaiCompatibleProvidersConfigMapName" -}}
{{- printf "%s-openai-providers" (include "oqto.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
