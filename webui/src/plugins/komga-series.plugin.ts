import KomgaSeriesService from '@/services/komga-series.service'
import { AxiosInstance } from 'axios'
import type {App} from 'vue'

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaSeries = new KomgaSeriesService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaSeries: KomgaSeriesService;
  }
}
