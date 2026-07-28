<template>
  <v-container fluid class="pa-6">
    <v-data-table-server
      :headers="headers"
      :items="books"
      v-model:options="options"
      :items-length="totalBooks"
      :loading="loading"
      multi-sort
      class="elevation-1"
      :items-per-page-options="[20, 50, 100]"
    >
      <template v-slot:item.seriesTitle="{ item }">
        <router-link
          :to="{name: item.oneshot ? 'browse-oneshot' : 'browse-series', params: {bookId: item.id, seriesId: item.seriesId}}">
          {{ item.seriesTitle }}
        </router-link>
      </template>

      <template v-slot:item.metadata.title="{ item }">
        <router-link
          :to="{name: item.oneshot ? 'browse-oneshot' : 'browse-book', params: {bookId: item.id, seriesId: item.seriesId}}">
          {{ item.metadata.title }}
        </router-link>
      </template>

      <template v-slot:item.deleted="{ item }">
        <v-chip
          v-if="item.deleted"
          label size="small" color="error">
          {{ $t('common.unavailable') }}
        </v-chip>
      </template>

      <template v-slot:footer.prepend>
        <v-btn icon @click="loadBooks">
          <v-icon>mdi-refresh</v-icon>
        </v-btn>
      </template>
    </v-data-table-server>
  </v-container>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {MediaStatus} from '@/types/enum-books'
import {BookDto} from '@/types/komga-books'
import {
  BookSearch,
  PosterMatch,
  SearchConditionAllOfBook,
  SearchConditionMediaStatus,
  SearchConditionPoster,
  SearchOperatorIs,
  SearchOperatorIsNot,
} from '@/types/komga-search'
import {createServerTableOptions, toPageRequest} from '@/functions/data-table'

export default defineComponent({
  name: 'MissingPosters',
  data: function () {
    return {
      books: [] as BookDto[],
      totalBooks: 0,
      loading: true,
      options: createServerTableOptions(20),
    }
  },
  watch: {
    options: {
      handler() {
        this.loadBooks()
      },
      deep: true,
    },
  },
  computed: {
    headers(): object[] {
      return [
        {title: this.$tc('common.series', 1).toString(), key: 'seriesTitle', sortable: false},
        {title: this.$t('common.book').toString(), key: 'metadata.title'},
        {title: this.$t('media_analysis.media_type').toString(), key: 'media.mediaType'},
        {title: this.$t('media_analysis.url').toString(), key: 'url'},
        {title: '', key: 'deleted', sortable: false},
      ]
    },
  },
  methods: {
    async loadBooks() {
      this.loading = true

      const pageRequest = toPageRequest(this.options)

      const booksPage = await this.$komgaBooks.getBooksList({
        condition: new SearchConditionAllOfBook([
          new SearchConditionMediaStatus(new SearchOperatorIs(MediaStatus.READY)),
          new SearchConditionPoster(new SearchOperatorIsNot({selected: true} as PosterMatch)),
        ]),
      } as BookSearch, pageRequest)
      this.totalBooks = booksPage.totalElements
      this.books = booksPage.content

      this.loading = false
    },
  },
})
</script>

<style scoped>

</style>
