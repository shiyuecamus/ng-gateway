{{/*
Expand the name of the chart.
*/}}
{{- define "ng-gateway.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "ng-gateway.fullname" -}}
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
{{- define "ng-gateway.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "ng-gateway.labels" -}}
helm.sh/chart: {{ include "ng-gateway.chart" . }}
{{ include "ng-gateway.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "ng-gateway.selectorLabels" -}}
app.kubernetes.io/name: {{ include "ng-gateway.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Gateway labels
*/}}
{{- define "ng-gateway.gateway.labels" -}}
helm.sh/chart: {{ include "ng-gateway.chart" . }}
{{ include "ng-gateway.gateway.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/component: gateway
{{- end }}

{{/*
Gateway selector labels
*/}}
{{- define "ng-gateway.gateway.selectorLabels" -}}
app.kubernetes.io/name: {{ include "ng-gateway.name" . }}-gateway
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: gateway
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "ng-gateway.serviceAccountName" -}}
{{- if .Values.gateway.serviceAccount.create }}
{{- default (include "ng-gateway.fullname" .) .Values.gateway.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.gateway.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Gateway image
*/}}
{{- define "ng-gateway.gateway.image" -}}
{{- $registry := .Values.gateway.image.registry | default .Values.global.imageRegistry -}}
{{- if $registry }}
{{- printf "%s/%s:%s" $registry .Values.gateway.image.repository (.Values.gateway.image.tag | default .Chart.AppVersion) }}
{{- else }}
{{- printf "%s:%s" .Values.gateway.image.repository (.Values.gateway.image.tag | default .Chart.AppVersion) }}
{{- end }}
{{- end }}

{{/*
Gateway image pull policy
*/}}
{{- define "ng-gateway.gateway.imagePullPolicy" -}}
{{- .Values.gateway.image.pullPolicy | default .Values.global.imagePullPolicy }}
{{- end }}

Storage class
*/}}
{{- define "ng-gateway.storageClass" -}}
{{- if .Values.global.storageClass }}
{{- .Values.global.storageClass }}
{{- else if .Values.persistence.gatewayData.storageClass }}
{{- .Values.persistence.gatewayData.storageClass }}
{{- else }}
{{- "" }}
{{- end }}
{{- end }}
