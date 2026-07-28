<template>
  <v-container fluid class="pa-6">
    <v-data-table-server
      :headers="headers"
      :items="booksData"
      v-model:options="options"
      :items-length="totalBooks"
      :loading="loading"
      multi-sort
      class="elevation-1"
      :items-per-page-options="[20, 50, 100]"
    >
      <template v-slot:top>
        <v-container>
          <v-row>
            <v-col cols="12" sm="6">
              <v-chip-group v-model="filterStatus" color="primary" mandatory multiple>
                <v-chip filter value="error">{{ $t('common.error') }}</v-chip>
                <v-chip filter value="unsupported">{{ $t('book_card.unsupported') }}</v-chip>
              </v-chip-group>
            </v-col>
            <v-col cols="12" sm="6">
              <v-select v-model="filterLibraries"
                        :items="filterLibrariesOptions"
                        :label="$t('navigation.libraries')"
                        clearable variant="solo"
                        multiple
                        chips
                        closable-chips
              />
            </v-col>
          </v-row>
        </v-container>
      </template>

      <template v-slot:item.libraryId="{ item }">
        {{ getLibraryName(item.libraryId) }}
      </template>

      <template v-slot:item.name="{ item }">
        <router-link
          :to="{name: item.oneshot ? 'browse-oneshot' : 'browse-book', params: {bookId: item.id, seriesId: item.seriesId}}">
          {{ item.name }}
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
import {convertErrorCodes} from '@/functions/error-codes'
import {
  BookSearch,
  SearchConditionAllOfBook,
  SearchConditionAnyOfBook,
  SearchConditionBook,
  SearchConditionLibraryId,
  SearchConditionMediaStatus,
  SearchOperatorIs,
} from '@/types/komga-search'
import {createServerTableOptions, toPageRequest} from '@/functions/data-table'

export default defineComponent({
  name: 'MediaAnalysis',
  data: function () {
    return {
      books: [] as BookDto[],
      totalBooks: 0,
      loading: true,
      options: createServerTableOptions(20, [{key: 'media.status', order: 'asc'}]),
      filterStatus: ['error', 'unsupported'],
      filterLibraries: [] as string[],
    }
  },
  watch: {
    options: {
      handler() {
        this.loadBooks()
      },
      deep: true,
    },
    filterStatus() {
      this.loadBooks()
    },
    filterLibraries() {
      this.loadBooks()
    },
  },
  computed: {
    filterLibrariesOptions(): object[] {
      return this.$store.state.komgaLibraries.libraries.map(x => ({
        text: x.name,
        value: x.id,
      }))
    },
    headers(): object[] {
      return [
        {title: this.$t('common.library').toString(), key: 'libraryId', sortable: false},
        {title: this.$t('media_analysis.name').toString(), key: 'name'},
        {title: this.$t('media_analysis.status').toString(), key: 'media.status'},
        {title: this.$t('media_analysis.comment').toString(), key: 'media.comment'},
        {title: this.$t('media_analysis.media_type').toString(), key: 'media.mediaType'},
        {title: this.$t('media_analysis.url').toString(), key: 'url'},
        {title: this.$t('media_analysis.size').toString(), key: 'size'},
        {title: '', key: 'deleted', sortable: false},
      ]
    },
    booksData(): BookDto[] {
      return this.books.map((b: BookDto) => ({
        ...b,
        media: {
          ...b.media,
          comment: convertErrorCodes(b.media.comment),
          status: this.$t(`enums.media_status.${b.media.status}`).toString(),
        },
      }))
    },
  },
  methods: {
    getLibraryName(libraryId: string): string {
      return this.$store.getters.getLibraryById(libraryId).name
    },
    async loadBooks() {
      this.loading = true

      const pageRequest = toPageRequest(this.options)

      const conditionsStatus = [] as SearchConditionBook[]
      if (this.filterStatus.includes('error')) conditionsStatus.push(new SearchConditionMediaStatus(new SearchOperatorIs(MediaStatus.ERROR)))
      if (this.filterStatus.includes('unsupported')) conditionsStatus.push(new SearchConditionMediaStatus(new SearchOperatorIs(MediaStatus.UNSUPPORTED)))

      const conditionsLibraries = [] as SearchConditionBook[]
      this.filterLibraries.forEach(x => conditionsLibraries.push(new SearchConditionLibraryId(new SearchOperatorIs(x))))

      if (this.filterStatus.length > 0) {
        const booksPage = await this.$komgaBooks.getBooksList({
          condition: new SearchConditionAllOfBook([
            new SearchConditionAnyOfBook(conditionsStatus),
            new SearchConditionAnyOfBook(conditionsLibraries),
          ]),
        } as BookSearch, pageRequest)
        this.totalBooks = booksPage.totalElements
        this.$store.commit('setBooksToCheck', booksPage.totalElements)
        this.books = booksPage.content
      }

      this.loading = false
    },
  },
})
</script>

<style scoped>

</style>
