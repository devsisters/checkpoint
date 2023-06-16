{{/*
Expand the name of the chart.
*/}}
{{- define "checkpoint.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "checkpoint.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "checkpoint.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "checkpoint.labels" -}}
helm.sh/chart: {{ include "checkpoint.chart" . }}
{{ include "checkpoint.selectorLabels.common" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "checkpoint.selectorLabels.common" -}}
app.kubernetes.io/name: {{ include "checkpoint.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "checkpoint.selectorLabels.controller" -}}
{{ include "checkpoint.selectorLabels.common" . }}
app.kubernetes.io/component: controller
{{- end }}

{{- define "checkpoint.selectorLabels.webhook" -}}
{{ include "checkpoint.selectorLabels.common" . }}
app.kubernetes.io/component: webhook
{{- end }}

{{- define "checkpoint.selfSignedIssuerName" -}}
{{ include "checkpoint.fullname" . }}-selfsigned
{{- end -}}

{{- define "checkpoint.caIssuerName" -}}
{{ include "checkpoint.fullname" . }}-ca
{{- end -}}
