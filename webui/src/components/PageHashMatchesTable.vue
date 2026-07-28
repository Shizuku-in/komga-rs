<template>
  <v-data-table-server
    :headers="headers"
    :items="elements"
    v-model:options="options"
    :items-length="totalElements"
    :loading="loading"
    class="elevation-1"
    :items-per-page-options="[10, 20, 50]"
  >
    <template v-slot:item.url="{ item }">
      <router-link :to="{name:'browse-book', params: {bookId: item.bookId}}">{{ item.url }}</router-link>
    </template>

    <template v-slot:item.bookId="{ item }">
      <v-img
        contain
        height="200"
        :src="bookPageThumbnailUrl(item.bookId, item.pageNumber)"
      />
    </template>

    <template v-slot:item.delete="{ item }">
      <v-btn
        icon
        color="error"
        @click="deleteMatch(item)"
      >
        <v-icon>mdi-trash-can-outline</v-icon>
      </v-btn>
    </template>

    <template v-slot:footer.prepend>
      <v-btn icon @click="loadData(hash)">
        <v-icon>mdi-refresh</v-icon>
      </v-btn>
    </template>
  </v-data-table-server>
</template>

<script lang="ts">
import {defineComponent, PropType} from 'vue'
import {bookPageThumbnailUrl} from '@/functions/urls'
import {PageHashDto, PageHashMatchDto} from '@/types/komga-pagehashes'
import {createServerTableOptions, toPageRequest} from '@/functions/data-table'

export default defineComponent({
  name: 'PageHashMatchesTable',
  props: {
    hash: {
      type: Object as PropType<PageHashDto>,
    },
  },
  data() {
    return {
      elements: [] as PageHashMatchDto[],
      totalElements: 0,
      loading: true,
      options: createServerTableOptions(10),
      bookPageThumbnailUrl,
    }
  },
  watch: {
    options: {
      handler() {
        this.loadData(this.hash)
      },
      deep: true,
    },
    hash() {
      this.options.page = 1
      this.options.sortBy = []
      this.elements = []
      this.totalElements = 0
    },
  },
  computed: {
    headers(): object[] {
      return [
        {title: this.$t('common.url').toString(), key: 'url'},
        {title: this.$t('common.filename').toString(), key: 'fileName'},
        {title: this.$t('common.page_number').toString(), key: 'pageNumber'},
        {title: this.$t('common.page').toString(), key: 'bookId'},
        {title: this.$t('menu.delete').toString(), key: 'delete'},
      ]
    },
  },
  methods: {
    async loadData(hash: PageHashDto) {
      this.loading = true

      const pageRequest = toPageRequest(this.options)

      const elementsPage = await this.$komgaPageHashes.getPageHashMatches(hash, pageRequest)
      this.totalElements = elementsPage.totalElements
      this.elements = elementsPage.content

      this.loading = false
    },
    async deleteMatch(match: PageHashMatchDto) {
      await this.$komgaPageHashes.deleteSingleMatch(this.hash, match)
    },
  },
})
</script>
