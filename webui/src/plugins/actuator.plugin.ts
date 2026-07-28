import ActuatorService from '@/services/actuator.service'
import { AxiosInstance } from 'axios'
import type {App} from 'vue'

export default {
  install (
    Vue: App,
    { http }: { http: AxiosInstance }) {
    Vue.config.globalProperties.$actuator = new ActuatorService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $actuator: ActuatorService;
  }
}
