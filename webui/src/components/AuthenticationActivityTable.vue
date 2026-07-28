<template>
  <v-data-table-server
    :headers="headers"
    :items="items"
    v-model:options="options"
    :items-length="totalItems"
    :loading="loading"
    multi-sort
    class="elevation-1"
    :items-per-page-options="[20, 50, 100]"
  >
    <template v-slot:item.success="{ item }">
      <v-icon v-if="item.success" color="success">mdi-check-circle</v-icon>
      <v-icon v-else color="error">mdi-alert-circle</v-icon>
    </template>

    <template v-slot:item.dateTime="{ item }">
      {{
        new Intl.DateTimeFormat($i18n.locale, {
          dateStyle: 'medium',
          timeStyle: 'short'
        }).format(item.dateTime)
      }}
    </template>
  </v-data-table-server>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {ERROR} from '@/types/events'
import {AuthenticationActivityDto} from '@/types/komga-users'
import {createServerTableOptions, toPageRequest} from '@/functions/data-table'

export default defineComponent({
  name: 'AuthenticationActivityTable',
  data: function () {
    return {
      items: [] as AuthenticationActivityDto[],
      totalItems: 0,
      loading: true,
      options: createServerTableOptions(20, [{key: 'dateTime', order: 'desc'}]),
    }
  },
  props: {
    forMe: {
      type: Boolean,
      default: false,
    },
  },
  watch: {
    options: {
      handler() {
        this.loadData()
      },
      deep: true,
    },
  },
  computed: {
    headers(): object[] {
      const headers = []
      if (!this.forMe) headers.push({title: this.$t('authentication_activity.email').toString(), key: 'email'})
      headers.push(
        {title: this.$t('authentication_activity.ip').toString(), key: 'ip'},
        {title: this.$t('authentication_activity.user_agent').toString(), key: 'userAgent'},
        {title: this.$t('authentication_activity.success').toString(), key: 'success'},
        {title: this.$t('authentication_activity.source').toString(), key: 'source'},
        {title: this.$t('authentication_activity.api_key').toString(), key: 'apiKeyComment'},
        {title: this.$t('authentication_activity.error').toString(), key: 'error'},
        {title: this.$t('authentication_activity.datetime').toString(), key: 'dateTime'},
      )
      return headers
    },
  },
  methods: {
    async loadData() {
      this.loading = true

      const pageRequest = toPageRequest(this.options)

      let itemsPage
      try {
        if (!this.forMe) itemsPage = await this.$komgaUsers.getAuthenticationActivity(pageRequest)
        else itemsPage = await this.$komgaUsers.getMyAuthenticationActivity(pageRequest)
        this.totalItems = itemsPage.totalElements
        this.items = itemsPage.content
      } catch (e) {
        this.$eventHub.$emit(ERROR, {message: e.message} as ErrorEvent)
      }

      this.loading = false
    },
  },
})
</script>
