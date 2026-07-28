import KomgaClaimService from '@/services/komga-claim.service'
import { AxiosInstance } from 'axios'
import type {App} from 'vue'

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaClaim = new KomgaClaimService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaClaim: KomgaClaimService;
  }
}
