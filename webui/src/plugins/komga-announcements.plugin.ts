import {AxiosInstance} from 'axios'
import type {App} from 'vue'
import KomgaAnnouncementsService from '@/services/komga-announcements.service'

export default {
  install(
    Vue: App,
    {http}: { http: AxiosInstance }) {
    Vue.config.globalProperties.$komgaAnnouncements = new KomgaAnnouncementsService(http)
  },
}

declare module '@vue/runtime-core' {
  interface ComponentCustomProperties {
    $komgaAnnouncements: KomgaAnnouncementsService;
  }
}
