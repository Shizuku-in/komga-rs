import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaMetricsService from '@/services/komga-metrics.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaMetrics = new KomgaMetricsService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaMetrics: KomgaMetricsService;
  }
}
