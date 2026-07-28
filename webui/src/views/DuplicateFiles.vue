<template>
  <v-container fluid class="pa-6">
    <v-data-table-server
      :headers="headers"
      :items="books"
      v-model:options="options"
      :items-length="totalBooks"
      :loading="loading"
      multi-sort
      :group-by="options.groupBy"
      class="elevation-1"
      :items-per-page-options="[20, 50, 100]"
    >
      <template v-slot:item.url="{ item }">
        <router-link
          :to="{name: item.oneshot ? 'browse-oneshot' : 'browse-book', params: {bookId: item.id, seriesId: item.seriesId}}">
          {{ item.url }}
        </router-link>
      </template>

      <template v-slot:item.deleted="{ item }">
        <v-chip
          v-if="item.deleted"
          label size="small" color="error">
          {{ $t('common.unavailable') }}
        </v-chip>
      </template>

      <template v-slot:item.id="{ item }">
        <v-btn
          icon
          color="error"
          @click="promptDeleteBook(item)"
        >
          <v-icon>mdi-trash-can-outline</v-icon>
        </v-btn>
      </template>

      <template v-slot:footer.prepend>
        <v-btn icon @click="loadBooks"><v-icon>mdi-refresh</v-icon></v-btn>
      </template>
    </v-data-table-server>
  </v-container>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {BookDto} from '@/types/komga-books'
import {createServerTableOptions, toPageRequest} from '@/functions/data-table'

export default defineComponent({
  name: 'DuplicateFiles',
  data: function () {
    return {
      books: [] as BookDto[],
      totalBooks: 0,
      loading: true,
      options: createServerTableOptions(20, [{key: 'fileHash', order: 'asc'}], [{key: 'fileHash', order: 'asc'}]),
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
        {title: this.$t('duplicates.file_hash').toString(), key: 'fileHash'},
        {title: this.$t('duplicates.url').toString(), key: 'url'},
        {title: this.$t('duplicates.size').toString(), key: 'size'},
        {title: '', key: 'deleted', sortable: false},
        {title: this.$t('menu.delete').toString(), key: 'id'},
      ]
    },
  },
  methods: {
    async loadBooks() {
      this.loading = true

      const pageRequest = toPageRequest(this.options)

      const booksPage = await this.$komgaBooks.getDuplicateBooks(pageRequest)
      this.totalBooks = booksPage.totalElements
      this.books = booksPage.content

      this.loading = false
    },
    promptDeleteBook(book: BookDto) {
      this.$store.dispatch('dialogDeleteBook', book)
    },
  },
})
</script>

<style scoped>

</style>
