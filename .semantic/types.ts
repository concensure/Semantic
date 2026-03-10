export type ProviderSettings = {
  name: string;
  model: string;
  apiKeyEnv?: string;
  endpoint: string;
  enabled: boolean;
};

export type SettingsPayload = {
  llm_config: string;
  llm_routing: string;
  model_metrics: string;
  env_file: string;
  enable_ollama: boolean;
};
