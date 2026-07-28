<template>
  <v-dialog v-model="modal"
            :fullscreen="$vuetify.breakpoint.xsOnly"
            max-width="800"
            @keydown.esc="dialogCancel"
  >
    <form novalidate>
      <v-card>
        <v-toolbar class="hidden-sm-and-up">
          <v-btn icon @click="dialogCancel">
            <v-icon>mdi-close</v-icon>
          </v-btn>
          <v-toolbar-title>{{ dialogTitle }}</v-toolbar-title>
          <v-spacer/>
          <v-toolbar-items>
            <v-btn variant="text" color="primary" @click="dialogConfirm">{{ $t('dialog.edit_books.button_confirm') }}</v-btn>
          </v-toolbar-items>
        </v-toolbar>

        <v-card-title class="hidden-xs">
          <v-icon class="mx-4">mdi-pencil</v-icon>
          {{ dialogTitle }}
        </v-card-title>

        <div :class="{'d-flex': $vuetify.breakpoint.smAndUp}">
          <v-tabs :direction="$vuetify.breakpoint.smAndUp ? 'vertical' : 'horizontal'" v-model="tab">
          <v-tab class="justify-start" v-if="single">
            <v-icon start class="hidden-xs">mdi-format-align-center</v-icon>
            {{ $t('dialog.edit_books.tab_general') }}
          </v-tab>
          <v-tab class="justify-start">
            <v-icon start class="hidden-xs">mdi-account-multiple</v-icon>
            {{ $t('dialog.edit_books.tab_authors') }}
          </v-tab>
          <v-tab class="justify-start">
            <v-icon start class="hidden-xs">mdi-tag-multiple</v-icon>
            {{ $t('dialog.edit_books.tab_tags') }}
          </v-tab>
          <v-tab class="justify-start" v-if="single">
            <v-icon start class="hidden-xs">mdi-link</v-icon>
            {{ $t('dialog.edit_books.tab_links') }}
          </v-tab>
          <v-tab class="justify-start" v-if="single">
            <v-icon start class="hidden-xs">mdi-image</v-icon>
            {{ $t('dialog.edit_books.tab_poster') }}
          </v-tab>
          </v-tabs>

          <v-window v-model="tab" class="flex-grow-1 tabbed-dialog-content">
          <!--  Tab: General  -->
          <v-window-item v-if="single">
            <v-card flat>
              <v-container fluid>

                <!--  Title  -->
                <v-row v-if="single">
                  <v-col cols="12">
                    <v-text-field v-model="form.title"
                                  :label="$t('dialog.edit_books.field_title')" variant="filled" density="compact"
                                  :error-messages="requiredErrors('title')"
                                  @update:model-value="v$.form.title.$touch(); form.titleLock = true"
                                  @blur="v$.form.title.$touch()"
                    >
                      <template v-slot:prepend>
                        <v-icon :color="form.titleLock ? 'secondary' : ''"
                                @click="form.titleLock = !form.titleLock"
                        >
                          {{ form.titleLock ? 'mdi-lock' : 'mdi-lock-open' }}
                        </v-icon>
                      </template>
                    </v-text-field>
                  </v-col>
                </v-row>

                <!--  Number  -->
                <v-row v-if="single">
                  <v-col cols="6">
                    <v-text-field v-model="form.number"
                                  :label="$t('dialog.edit_books.field_number')" variant="filled" density="compact"
                                  :error-messages="requiredErrors('number')"
                                  @update:model-value="v$.form.number.$touch(); form.numberLock = true"
                                  @blur="v$.form.number.$touch()"
                    >
                      <template v-slot:prepend>
                        <v-icon :color="form.numberLock ? 'secondary' : ''"
                                @click="form.numberLock = !form.numberLock"
                        >
                          {{ form.numberLock ? 'mdi-lock' : 'mdi-lock-open' }}
                        </v-icon>
                      </template>
                    </v-text-field>
                  </v-col>

                  <!--  Sort Number  -->
                  <v-col cols="6">
                    <v-text-field v-model="form.numberSort"
                                  type="number"
                                  step="0.1"
                                  :label="$t('dialog.edit_books.field_number_sort')" variant="filled" density="compact"
                                  :hint="$t('dialog.edit_books.field_number_sort_hint')"
                                  persistent-hint
                                  :error-messages="requiredErrors('numberSort')"
                                  @update:model-value="v$.form.numberSort.$touch(); form.numberSortLock = true"
                                  @blur="v$.form.numberSort.$touch()"
                    >
                      <template v-slot:prepend>
                        <v-icon :color="form.numberSortLock ? 'secondary' : ''"
                                @click="form.numberSortLock = !form.numberSortLock"
                        >
                          {{ form.numberSortLock ? 'mdi-lock' : 'mdi-lock-open' }}
                        </v-icon>
                      </template>
                    </v-text-field>
                  </v-col>
                </v-row>

                <!--  Summary  -->
                <v-row v-if="single">
                  <v-col cols="12">
                    <v-textarea v-model="form.summary"
                                :label="$t('dialog.edit_books.field_summary')" variant="filled" density="compact"
                                @update:model-value="v$.form.summary.$touch(); form.summaryLock = true"
                    >
                      <template v-slot:prepend>
                        <v-icon :color="form.summaryLock ? 'secondary' : ''"
                                @click="form.summaryLock = !form.summaryLock"
                        >
                          {{ form.summaryLock ? 'mdi-lock' : 'mdi-lock-open' }}
                        </v-icon>
                      </template>
                    </v-textarea>
                  </v-col>
                </v-row>

                <v-row v-if="single">
                  <!--  Release Date  -->
                  <v-col cols="6">
                    <v-menu
                      v-model="menuDate"
                      :close-on-content-click="false"
                      transition="scale-transition" location="bottom"
                      min-width="auto"
                    >
                      <template v-slot:activator="{ props }">
                        <v-text-field v-model="form.releaseDate"
                                      :label="$t('dialog.edit_books.field_release_date')" variant="filled" density="compact"
                                      placeholder="YYYY-MM-DD"
                                      clearable
                                      :error-messages="releaseDateErrors"
                                      @blur="v$.form.releaseDate.$touch()"
                                      @update:model-value="form.releaseDateLock = true"
                                      v-bind="props"
                        >
                          <template v-slot:prepend>
                            <v-icon :color="form.releaseDateLock ? 'secondary' : ''"
                                    @click="form.releaseDateLock = !form.releaseDateLock"
                            >
                              {{ form.releaseDateLock ? 'mdi-lock' : 'mdi-lock-open' }}
                            </v-icon>
                          </template>
                        </v-text-field>
                      </template>
                      <v-locale-provider :locale="$i18n.locale">
                        <v-date-picker v-model="form.releaseDate"
                                       :show-adjacent-months="true"
                                       :first-day-of-week="$store.getters.getLocaleFirstDay()"
                                       @update:model-value="menuDate = false; form.releaseDateLock = true"
                        />
                      </v-locale-provider>
                    </v-menu>
                  </v-col>

                  <!--  ISBN  -->
                  <v-col cols="6">
                    <v-text-field v-model.trim="form.isbn"
                                  :label="$t('dialog.edit_books.field_isbn')" variant="filled" density="compact"
                                  placeholder="978-2-20-504375-4"
                                  clearable
                                  :error-messages="isbnErrors"
                                  @blur="v$.form.isbn.$touch()"
                                  @update:model-value="form.isbnLock = true"
                    >
                      <template v-slot:prepend>
                        <v-icon :color="form.isbnLock ? 'secondary' : ''"
                                @click="form.isbnLock = !form.isbnLock"
                        >
                          {{ form.isbnLock ? 'mdi-lock' : 'mdi-lock-open' }}
                        </v-icon>
                      </template>
                    </v-text-field>
                  </v-col>
                </v-row>

              </v-container>
            </v-card>
          </v-window-item>

          <!--  Tab: Authors  -->
          <v-window-item>
            <v-card flat>
              <v-container fluid>
                <v-alert v-if="!single"
                         type="warning" variant="outlined" density="compact"
                >{{ $t('dialog.edit_books.authors_notice_multiple_edit') }}
                </v-alert>
                <v-row v-for="(role, i) in authorRoles"
                       :key="i"
                >
                  <v-col cols="12">
                    <span class="text-body-2">{{ $_.capitalize(role.name) }}</span>
                    <v-combobox v-model="form.authors[role.value]"
                                :items="authorSearchResultsFull"
                                v-model:search="authorSearch[i]"
                                @keydown.esc="authorSearch[i] = null"
                                @update:model-value="v$.form.authors.$touch(); form.authorsLock = true"
                                hide-selected
                                chips
                                closable-chips
                                multiple variant="filled" density="compact"
                    >
                      <template v-slot:prepend>
                        <v-icon :color="form.authorsLock ? 'secondary' : ''"
                                @click="form.authorsLock = !form.authorsLock"
                        >
                          {{ form.authorsLock ? 'mdi-lock' : 'mdi-lock-open' }}
                        </v-icon>
                      </template>
                    </v-combobox>
                  </v-col>
                </v-row>
                <v-row>
                  <v-col cols="12" md="6">
                    <v-form
                      v-model="customRoleValid"
                      ref="customRoleForm"
                      @submit.prevent="addRole"
                    >
                      <v-text-field
                        label="Add custom role"
                        append-icon="mdi-plus"
                        v-model.trim="customRole"
                        @click:append="addRole"
                        :rules="customRoleRules"
                      ></v-text-field>
                    </v-form>
                  </v-col>
                </v-row>
              </v-container>
            </v-card>
          </v-window-item>

          <!--  Tab: Tags  -->
          <v-window-item>
            <v-card flat>
              <v-container fluid>
                <v-alert v-if="!single"
                         type="warning" variant="outlined" density="compact"
                >{{ $t('dialog.edit_books.tags_notice_multiple_edit') }}
                </v-alert>

                <!-- Tags -->
                <v-row>
                  <v-col cols="12">
                    <span class="text-body-2">{{ $t('dialog.edit_books.field_tags') }}</span>
                    <v-combobox v-model="form.tags"
                                :items="tagsAvailable"
                                @update:model-value="v$.form.tags.$touch(); form.tagsLock = true"
                                hide-selected
                                chips
                                closable-chips
                                multiple variant="filled" density="compact"
                    >
                      <template v-slot:prepend>
                        <v-icon :color="form.tagsLock ? 'secondary' : ''"
                                @click="form.tagsLock = !form.tagsLock"
                        >
                          {{ form.tagsLock ? 'mdi-lock' : 'mdi-lock-open' }}
                        </v-icon>
                      </template>
                    </v-combobox>
                  </v-col>
                </v-row>
              </v-container>
            </v-card>
          </v-window-item>

          <!--  Tab: Links  -->
          <v-window-item v-if="single">
            <v-card flat min-height="100">
              <v-container fluid>
                <!-- Links -->
                <v-form
                  v-model="linksValid"
                  ref="linksForm"
                >
                  <v-row
                    v-for="(link, i) in form.links"
                    :key="i"
                  >
                    <v-col cols="4" class="py-0">
                      <v-text-field v-model="form.links[i].label"
                                    :label="$t('dialog.edit_books.field_link_label')" variant="filled" density="compact"
                                    :rules="[linksLabelRules]"
                                    @update:model-value="v$.form.links.$touch(); form.linksLock = true"
                                    @blur="v$.form.links.$touch()"
                      >
                        <template v-slot:prepend>
                          <v-icon :color="form.linksLock ? 'secondary' : ''"
                                  @click="form.linksLock = !form.linksLock"
                          >
                            {{ form.linksLock ? 'mdi-lock' : 'mdi-lock-open' }}
                          </v-icon>
                        </template>
                      </v-text-field>
                    </v-col>

                    <v-col cols="8" class="py-0">
                      <v-text-field v-model="form.links[i].url"
                                    :label="$t('dialog.edit_books.field_link_url')" variant="filled" density="compact"
                                    :rules="[linksUrlRules]"
                                    @update:model-value="v$.form.links.$touch(); form.linksLock = true"
                                    @blur="v$.form.links.$touch()"
                      >
                        <template v-slot:append>
                          <v-icon @click="form.links.splice(i, 1)">mdi-delete</v-icon>
                        </template>
                      </v-text-field>
                    </v-col>
                  </v-row>
                </v-form>

                <v-row>
                  <v-spacer/>
                  <v-col cols="auto">
                    <v-btn
                      elevation="2"
                      icon
                      size="small"
                      color="primary"
                      @click="form.links.push({label:'', url:''})"
                    >
                      <v-icon>mdi-plus</v-icon>
                    </v-btn>
                  </v-col>
                </v-row>
              </v-container>
            </v-card>
          </v-window-item>

          <!--  Tab: Thumbnails  -->
          <v-window-item v-if="single">
            <v-card flat>
              <v-container fluid>
                <!-- Upload -->
                <v-row>
                  <v-col class="pa-1">
                    <drop-zone ref="thumbnailsUpload" @on-input-change="addThumbnail" class="pa-8"/>
                  </v-col>
                </v-row>

                <!-- Gallery -->
                <v-row>
                  <v-col
                    cols="6" sm="4" lg="3" class="pa-1"
                    v-for="(item, index) in [...poster.uploadQueue, ...poster.bookThumbnails]"
                    :key="index"
                  >
                    <thumbnail-card
                      :item="item"
                      :selected="isThumbnailSelected(item)"
                      :toBeDeleted="isThumbnailToBeDeleted(item)"
                      @on-select-thumbnail="selectThumbnail"
                      @on-delete-thumbnail="deleteThumbnail"
                    />
                  </v-col>
                </v-row>

              </v-container>
            </v-card>
          </v-window-item>
          </v-window>
        </div>

        <v-card-actions class="hidden-xs">
          <v-spacer/>
          <v-btn variant="text" @click="dialogCancel">{{ $t('dialog.edit_books.button_cancel') }}</v-btn>
          <v-btn color="primary" @click="dialogConfirm">{{ $t('dialog.edit_books.button_confirm') }}</v-btn>
        </v-card-actions>
      </v-card>
    </form>
  </v-dialog>
</template>

<script lang="ts">
import {groupAuthorsByRole} from '@/functions/authors'
import {authorRoles} from '@/types/author-roles'
import {defineComponent} from 'vue'
import useVuelidate from '@vuelidate/core'
import {helpers, requiredIf} from '@vuelidate/validators'
import {BookDto, BookThumbnailDto} from '@/types/komga-books'
import IsbnVerify from '@w0s/isbn-verify'
import {isMatch} from 'date-fns'
import {debounce} from 'lodash'
import {ERROR, ErrorEvent} from '@/types/events'
import DropZone from '@/components/DropZone.vue'
import ThumbnailCard from '@/components/ThumbnailCard.vue'
import {NameValue} from '@/types/filter'

const validDate = (value: string) => !helpers.req(value) || isMatch(value, 'yyyy-MM-dd') && value.length == 10
const validIsbn = (value: string) => !helpers.req(value) || new IsbnVerify(value).isIsbn13({check_digit: true})

export default defineComponent({
  name: 'EditBooksDialog',
  setup: () => ({v$: useVuelidate()}),
  components: {ThumbnailCard, DropZone},
  data: () => {
    return {
      modal: false,
      tab: 0,
      menuDate: false,
      customRole: '',
      customRoles: [] as string[],
      customRoleValid: false,
      linksValid: false,
      form: {
        title: '',
        titleLock: false,
        summary: '',
        summaryLock: false,
        number: '',
        numberLock: false,
        numberSort: 0,
        numberSortLock: false,
        releaseDate: '',
        releaseDateLock: false,
        authors: {},
        authorsLock: false,
        tags: [] as string[],
        tagsLock: false,
        isbn: '',
        isbnLock: false,
        links: [],
        linksLock: false,
      },
      poster: {
        selectedThumbnail: '',
        uploadQueue: [] as File[],
        deleteQueue: [] as BookThumbnailDto[],
        bookThumbnails: [] as BookThumbnailDto[],
      },
      authorSearch: [],
      authorSearchResults: [] as string[],
      tagsAvailable: [] as string[],
    }
  },
  props: {
    modelValue: Boolean,
    books: {
      type: [Object as () => BookDto, Array as () => BookDto[]],
      required: true,
    },
  },
  watch: {
    modelValue(val) {
      this.modal = val
    },
    modal(val) {
      if (val) {
        this.getThumbnails(this.books)
        this.loadAvailableTags()
      } else {
        this.dialogCancel()
      }
    },
    books: {
      immediate: true,
      handler(val) {
        this.dialogReset(val)
      },
    },
    authorSearch: {
      deep: true,
      handler: debounce(async function (this: any, val: []) {
        const index = val.findIndex(x => x !== null)
        this.authorSearchResults = await this.$komgaReferential.getAuthorsNames(val[index])
      }, 500),
    },
  },
  validations: {
    form: {
      title: {
        required: requiredIf(function (this: any, model: any) {
          return this.single
        }),
      },
      number: {
        required: requiredIf(function (this: any, model: any) {
          return this.single
        }),
      },
      numberSort: {
        required: requiredIf(function (this: any, model: any) {
          return this.single
        }),
      },
      tags: {},
      releaseDate: {validDate},
      summary: {},
      authors: {},
      isbn: {validIsbn},
      links: {},
    },
  },
  computed: {
    authorRoles(): NameValue[] {
      let remoteRoles = [] as string[]
      if (Array.isArray(this.books))
        remoteRoles = this.books.flatMap(b => b.metadata.authors).map(a => a.role)
      else if (this.books?.metadata?.authors)
        remoteRoles = this.books.metadata.authors.map(a => a.role)
      const allRoles = this.$_.uniq([...authorRoles, ...remoteRoles, ...this.customRoles])
      return allRoles.map((role: string) => ({
        name: this.$te(`author_roles.${role}`) ? this.$t(`author_roles.${role}`).toString() : role,
        value: role,
      }))
    },
    single(): boolean {
      return !Array.isArray(this.books)
    },
    authorSearchResultsFull(): string[] {
      // merge local values with server search, so that already input value is available
      const local = (this.$_.values(this.form.authors).flat()) as unknown as string[]
      // eslint-disable-next-line vue/no-side-effects-in-computed-properties
      return this.$_.sortBy(this.$_.union(local, this.authorSearchResults), x => x.toLowerCase())
    },
    dialogTitle(): string {
      return this.single
        ? this.$t('dialog.edit_books.dialog_title_single', {book: this.$_.get(this.books, 'metadata.title')}).toString()
        : this.$tc('dialog.edit_books.dialog_title_multiple', (this.books as BookDto[]).length)
    },
    releaseDateErrors(): string[] {
      const errors = [] as string[]
      if (!this.v$.form?.releaseDate?.$dirty) return errors
      !this.v$?.form?.releaseDate?.validDate && errors.push(this.$t('dialog.edit_books.field_release_date_error').toString())
      return errors
    },
    isbnErrors(): string[] {
      const errors = [] as string[]
      if (!this.v$.form?.isbn?.$dirty) return errors
      !this.v$?.form?.isbn?.validIsbn && errors.push(this.$t('dialog.edit_books.field_isbn_error').toString())
      return errors
    },
    customRoleRules(): any[] {
      if (this.customRole === '') return [this.$t('common.required').toString()]
      if (this.authorRoles.map(n => n.value.toLowerCase()).includes(this.customRole?.toLowerCase())) return [this.$t('dialog.edit_books.add_author_role_error_duplicate').toString()]
      return [true]
    },
  },
  methods: {
    async loadAvailableTags() {
      this.tagsAvailable = await this.$komgaReferential.getTags()
    },
    linksLabelRules(label: string): boolean | string {
      if (!!this.$_.trim(label)) return true
      return this.$t('common.required').toString()
    },
    linksUrlRules(value: string): boolean | string {
      let url
      try {
        url = new URL(value)
      } catch (_) {
        return this.$t('dialog.edit_books.field_link_url_error_url').toString()
      }
      if (url.protocol === 'http:' || url.protocol === 'https:') return true
      return this.$t('dialog.edit_books.field_link_url_error_protocol').toString()
    },
    async addRole() {
      const {valid} = await (this.$refs.customRoleForm as any).validate()
      if (valid) {
        this.customRoles.push(this.customRole.toLowerCase());
        (this.$refs.customRoleForm as any).reset()
      }
    },
    requiredErrors(fieldName: string): string[] {
      const errors = [] as string[]
      const formField = this.v$.form!![fieldName] as any
      if (!formField.$dirty) return errors
      !formField.required && errors.push(this.$t('common.required').toString())
      return errors
    },
    dialogReset(books: BookDto | BookDto[]) {
      this.tab = 0;
      (this.$refs.customRoleForm as any)?.reset();
      (this.$refs.linksForm as any)?.resetValidation()
      this.customRoles = []
      this.v$.$reset?.()
      if (Array.isArray(books) && books.length === 0) return
      else if (this.$_.isEmpty(books)) return
      if (Array.isArray(books) && books.length > 0) {
        this.form.authors = {}
        this.form.links = []

        const authorsLock = this.$_.uniq(books.map(x => x.metadata.authorsLock))
        this.form.authorsLock = authorsLock.length > 1 ? false : authorsLock[0]

        this.form.tags = []

        const tagsLock = this.$_.uniq(books.map(x => x.metadata.tagsLock))
        this.form.tagsLock = tagsLock.length > 1 ? false : tagsLock[0]
      } else {
        this.form.tags = []
        this.form.links = []
        const book = books as BookDto
        this.$_.merge(this.form, book.metadata)
        this.form.authors = groupAuthorsByRole(book.metadata.authors)
        this.poster.selectedThumbnail = ''
        this.poster.deleteQueue = []
        this.poster.uploadQueue = []
        this.poster.bookThumbnails = []
      }
    },
    dialogCancel() {
      this.$emit('update:modelValue', false)
      this.dialogReset(this.books)
    },
    async dialogConfirm() {
      if (await this.editBooks()) {
        this.$emit('update:modelValue', false)
      }
    },
    async validateForm(): Promise<any> {
      if (this.v$.$invalid) return null
      if (this.single && this.$refs.linksForm) {
        const {valid} = await (this.$refs.linksForm as any).validate()
        if (!valid) return null
      }
      {
        const metadata = {
          authorsLock: this.form.authorsLock,
          tagsLock: this.form.tagsLock,
        }

        if (this.v$.form?.authors?.$dirty) {
          this.$_.merge(metadata, {
            authors: this.$_.keys(this.form.authors).flatMap((role: string) =>
              this.$_.get(this.form.authors, role).map((name: string) => ({name: name, role: role})),
            ),
          })
        }

        if (this.v$.form?.tags?.$dirty) {
          this.$_.merge(metadata, {tags: this.form.tags})
        }

        if (this.single) {
          this.$_.merge(metadata, {
            titleLock: this.form.titleLock,
            numberLock: this.form.numberLock,
            numberSortLock: this.form.numberSortLock,
            summaryLock: this.form.summaryLock,
            releaseDateLock: this.form.releaseDateLock,
            isbnLock: this.form.isbnLock,
            linksLock: this.form.linksLock,
          })

          if (this.v$.form?.title?.$dirty) {
            this.$_.merge(metadata, {title: this.form.title})
          }

          if (this.v$.form?.number?.$dirty) {
            this.$_.merge(metadata, {number: this.form.number})
          }

          if (this.v$.form?.numberSort?.$dirty) {
            this.$_.merge(metadata, {numberSort: this.form.numberSort})
          }

          if (this.v$.form?.summary?.$dirty) {
            this.$_.merge(metadata, {summary: this.form.summary})
          }

          if (this.v$.form?.releaseDate?.$dirty) {
            this.$_.merge(metadata, {releaseDate: this.form.releaseDate ? this.form.releaseDate : null})
          }

          if (this.v$.form?.isbn?.$dirty) {
            this.$_.merge(metadata, {isbn: this.form.isbn})
          }

          if (this.v$.form?.links?.$dirty || this.form.links.length != (this.books as BookDto).metadata.links?.length) {
            this.$_.merge(metadata, {links: this.form.links})
          }
        }

        return metadata
      }
      return null
    },
    async editBooks(): Promise<boolean> {
      if (this.single && this.poster.uploadQueue.length > 0) {
        const book = this.books as BookDto
        let hadErrors = false
        for (const file of this.poster.uploadQueue.slice()) {
          try {
            await this.$komgaBooks.uploadThumbnail(book.id, file, file.name === this.poster.selectedThumbnail)
            this.deleteThumbnail(file)
          } catch (e) {
            this.$eventHub.$emit(ERROR, {message: e.message} as ErrorEvent)
            hadErrors = true
          }
        }
        if (hadErrors) {
          await this.getThumbnails(book)
          return false
        }
      }

      if (this.single && this.poster.selectedThumbnail !== '') {
        const id = this.poster.selectedThumbnail
        const book = this.books as BookDto
        if (this.poster.bookThumbnails.find(value => value.id === id)) {
          await this.$komgaBooks.markThumbnailAsSelected(book.id, id)
        }
      }

      if (this.single && this.poster.deleteQueue.length > 0) {
        this.poster.deleteQueue.forEach(toDelete => this.$komgaBooks.deleteThumbnail(toDelete.bookId, toDelete.id))
      }

      const metadata = await this.validateForm()
      if (metadata) {
        const toUpdate = (this.single ? [this.books] : this.books) as BookDto[]
        for (const b of toUpdate) {
          try {
            await this.$komgaBooks.updateMetadata(b.id, metadata)
          } catch (e) {
            this.$eventHub.$emit(ERROR, {message: e.message} as ErrorEvent)
          }
        }
        return true
      } else return false
    },
    addThumbnail(files: File[]) {
      let hasSelected = false
      for (const file of files) {
        if (!this.poster.uploadQueue.find(value => value.name === file.name)) {
          this.poster.uploadQueue.push(file)
          if (!hasSelected) {
            this.selectThumbnail(file)
            hasSelected = true
          }
        }
      }

      (this.$refs.thumbnailsUpload as any).reset()
    },
    async getThumbnails(book: BookDto | BookDto[]) {
      if (Array.isArray(book)) return

      const thumbnails = await this.$komgaBooks.getThumbnails(book.id)

      this.selectThumbnail(thumbnails.find(x => x.selected))

      this.poster.bookThumbnails = thumbnails
    },
    isThumbnailSelected(item: File | BookThumbnailDto): boolean {
      return item instanceof File ? item.name === this.poster.selectedThumbnail : item.id === this.poster.selectedThumbnail
    },
    selectThumbnail(item: File | BookThumbnailDto | undefined) {
      if (!item) {
        return
      } else if (item instanceof File) {
        this.poster.selectedThumbnail = item.name
      } else {
        const index = this.poster.deleteQueue.indexOf(item, 0)
        if (index > -1) this.poster.deleteQueue.splice(index, 1)

        this.poster.selectedThumbnail = item.id
      }
    },
    isThumbnailToBeDeleted(item: File | BookThumbnailDto) {
      if (item instanceof File) {
        return false
      } else {
        return this.poster.deleteQueue.includes(item)
      }
    },
    deleteThumbnail(item: File | BookThumbnailDto) {
      if (item instanceof File) {
        const index = this.poster.uploadQueue.indexOf(item, 0)
        if (index > -1) {
          this.poster.uploadQueue.splice(index, 1)
        }
        if (item.name === this.poster.selectedThumbnail) {
          this.poster.selectedThumbnail = ''
        }
      } else {
        // if thumbnail was marked for deletion, unmark it
        if (this.isThumbnailToBeDeleted(item)) {
          const index = this.poster.deleteQueue.indexOf(item, 0)
          if (index > -1) {
            this.poster.deleteQueue.splice(index, 1)
          }
        } else {
          this.poster.deleteQueue.push(item)
          if (item.id === this.poster.selectedThumbnail) this.poster.selectedThumbnail = ''
        }
      }
    },
  },
})
</script>

<style lang="sass" scoped>
@use '../../styles/tabbed-dialog'
</style>
